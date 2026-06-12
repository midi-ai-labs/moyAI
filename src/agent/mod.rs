//! Phase14 core rebuild: thin agent loop boundary.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::ResolvedConfig;
use crate::error::AgentError;
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelContentPart, ModelMessage, ModelProfile,
    ModelToolCall, ToolSchema,
};
use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload, TurnId};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, FinishReason, MessageId, MessageMetadata, MessagePart, MessageRole,
    NewMessage, NewPart, PartKind, RunEvent, RunSummary, SessionContext, SessionStateSnapshot,
    SessionStatus, TextPart, TokenUsage,
};
use crate::storage::StoreBundle;
use crate::tool::ToolResult;
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;

#[derive(Debug, Default, Clone, Copy)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(&self, request: &AgentRunRequest, tools: &[ToolSchema]) -> String {
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let tools = if tool_names.is_empty() {
            "none"
        } else {
            tool_names.as_str()
        };
        format!(
            "{}\n\nEnvironment:\n- workspace: {}\n- cwd: {}\n- access: {:?}\n- model: {}\n- tools: {}",
            include_str!("../../assets/prompts/system.md").trim(),
            request.session.workspace.root,
            request.session.workspace.cwd,
            request.config.permissions.access_mode,
            request.model.name,
            tools
        )
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeInputView {
    pub history_items: Vec<HistoryItem>,
}

impl RuntimeInputView {
    pub fn from_history_items(history_items: Vec<HistoryItem>) -> Self {
        Self { history_items }
    }

    pub fn has_user_turn(&self) -> bool {
        self.history_items
            .iter()
            .any(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
    }
}

pub struct AgentRunRequest {
    pub session: SessionContext,
    pub user_message_id: MessageId,
    pub protocol_turn_id: TurnId,
    pub runtime_input: RuntimeInputView,
    pub state: SessionStateSnapshot,
    pub config: ResolvedConfig,
    pub model: ModelProfile,
    pub cancel: CancellationToken,
}

#[derive(Clone)]
pub struct AgentLoop {
    llm: Arc<dyn LlmClient>,
    registry: ToolRegistry,
    store: StoreBundle,
    prompt_builder: PromptBuilder,
    tool_services: ToolServices,
}

impl AgentLoop {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        registry: ToolRegistry,
        store: StoreBundle,
        prompt_builder: PromptBuilder,
        tool_services: ToolServices,
    ) -> Self {
        Self {
            llm,
            registry,
            store,
            prompt_builder,
            tool_services,
        }
    }

    pub async fn run(
        &self,
        request: AgentRunRequest,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        let repo = self.store.session_repo();
        let (assistant, started) = repo
            .append_assistant_message_with_protocol_start(
                NewMessage {
                    session_id: request.session.session.id,
                    parent_message_id: Some(request.user_message_id),
                    role: MessageRole::Assistant,
                    metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                        model: request.model.name.clone(),
                        base_url: request.config.model.base_url.clone(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                request.protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
                request.model.name.clone(),
            )
            .await?;
        sink.emit_pre_recorded(started)?;

        let tool_schemas = self.tool_schemas();
        let mut messages = messages_from_history(&request.runtime_input.history_items);
        let mut guard = LoopGuard::new(request.config.session.max_steps_per_turn);
        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut latest_usage: Option<TokenUsage> = None;

        let outcome: Result<RunSummary, AgentError> = async {
            loop {
                guard.check_step_budget()?;
                if request.cancel.is_cancelled() {
                    return self
                        .interrupt(
                            &request,
                            assistant.id,
                            latest_usage.clone(),
                            "run cancelled by user",
                            sink,
                        )
                        .await;
                }

                let chat_request = self.chat_request(&request, &messages, &tool_schemas)?;
                let mut collector = ResponseCollector::default();
                let response = self
                    .llm
                    .stream_chat(chat_request, request.cancel.clone(), &mut collector)
                    .await?;
                latest_usage = response.usage.clone();

                for reasoning in collector.reasoning {
                    sink.emit(RunEvent::ReasoningDelta {
                        message_id: assistant.id,
                        delta: reasoning,
                    })?;
                }
                if !collector.text.is_empty() {
                    persist_text_delta(
                        &repo,
                        request.session.session.id,
                        assistant.id,
                        request.protocol_turn_id,
                        sink,
                        collector.text.clone(),
                    )
                    .await?;
                }

                if response.finish_reason == FinishReason::Cancelled {
                    return self
                        .interrupt(
                            &request,
                            assistant.id,
                            latest_usage.clone(),
                            "run cancelled by user",
                            sink,
                        )
                        .await;
                }

                if collector.tool_calls.is_empty() {
                    let event = RunEvent::SessionCompleted {
                        session_id: request.session.session.id,
                        finish_reason: Some(response.finish_reason),
                    };
                    let metadata =
                        assistant_metadata(&request, Some(response.finish_reason), response.usage);
                    repo.update_message_metadata_and_status_with_protocol_event(
                        request.session.session.id,
                        assistant.id,
                        &metadata,
                        SessionStatus::Completed,
                        &event,
                        request.protocol_turn_id,
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?;
                    sink.emit_pre_recorded(event)?;
                    return Ok(RunSummary {
                        session_id: request.session.session.id,
                        assistant_message_id: Some(assistant.id),
                        status: SessionStatus::Completed,
                        finish_reason: Some(response.finish_reason),
                        tool_call_count,
                        failed_tool_count,
                        change_count,
                    });
                }

                messages.push(ModelMessage::AssistantToolCalls {
                    content: (!collector.text.trim().is_empty()).then_some(collector.text),
                    tool_calls: collector.tool_calls.clone(),
                });

                for call in collector.tool_calls {
                    guard.record_tool_call(&call)?;
                    tool_call_count += 1;
                    let tool_output = self
                        .handle_tool_call(
                            &request,
                            assistant.id,
                            &tool_schemas,
                            call.clone(),
                            prompt,
                            sink,
                        )
                        .await?;
                    if tool_output.failed {
                        failed_tool_count += 1;
                    }
                    change_count += tool_output.change_count;
                    messages.push(ModelMessage::Tool {
                        call_id: call.call_id,
                        tool_name: call.tool_name,
                        result: tool_output.result_text,
                        metadata: Value::Null,
                    });
                }
            }
        }
        .await;

        match outcome {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.fail(
                    &request,
                    assistant.id,
                    latest_usage.clone(),
                    error.to_string(),
                    sink,
                )
                .await?;
                Err(error)
            }
        }
    }

    fn chat_request(
        &self,
        request: &AgentRunRequest,
        messages: &[ModelMessage],
        tools: &[ToolSchema],
    ) -> Result<ChatRequest, AgentError> {
        let chat_request = ChatRequest {
            model: request.model.clone(),
            base_url: request.config.model.base_url.clone(),
            system_prompt: self.prompt_builder.build(request, tools),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            tool_choice: None,
            parallel_tool_calls: crate::llm::effective_parallel_tool_calls(
                tools.len(),
                request.config.model.parallel_tool_calls,
                request.config.model.max_parallel_predictions,
            ),
            timeout_ms: request.config.model.request_timeout_ms,
            stream_idle_timeout_ms: request.config.model.stream_idle_timeout_ms,
            stream_max_retries: request.config.model.stream_max_retries,
            extra_headers: request.config.model.extra_headers.clone(),
            temperature: request.config.model.temperature,
            top_p: request.config.model.top_p,
            top_k: request.config.model.top_k,
            presence_penalty: request.config.model.presence_penalty,
            frequency_penalty: request.config.model.frequency_penalty,
            seed: request.config.model.seed,
            stop_sequences: request.config.model.stop_sequences.clone(),
            extra_body: request.config.model.extra_body_json.clone(),
        };
        chat_request.validate_provider_lifecycle()?;
        Ok(chat_request)
    }

    async fn handle_tool_call(
        &self,
        request: &AgentRunRequest,
        assistant_message_id: MessageId,
        schemas: &[ToolSchema],
        call: ModelToolCall,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolOutputForModel, AgentError> {
        let repo = self.store.session_repo();
        let parsed_arguments = parse_tool_arguments(&call.arguments_json)
            .and_then(|value| validate_shallow_schema(&call.tool_name, value, schemas));
        let (arguments, validation_error) = match parsed_arguments {
            Ok(value) => (value, None),
            Err(error) => (Value::Null, Some(error.to_string())),
        };
        let metadata = tool_route_metadata(&call, &arguments, schemas);
        let (record, pending) = repo
            .record_pending_tool_call_with_protocol_bundle(
                request.session.session.id,
                assistant_message_id,
                &call.tool_name,
                &call.arguments_json,
                Some(&call.tool_name),
                metadata.clone(),
                request.protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(pending)?;

        if let Some(error_text) = validation_error {
            let result_text = format!("invalid arguments for `{}`: {error_text}", call.tool_name);
            let failed = repo
                .fail_tool_call_with_protocol_bundle(
                    request.session.session.id,
                    assistant_message_id,
                    record.id,
                    record.tool_name,
                    &result_text,
                    failed_tool_metadata(metadata),
                    request.protocol_turn_id,
                    sink.reserve_protocol_sequence_no(),
                )
                .await?;
            sink.emit_pre_recorded(failed)?;
            return Ok(ToolOutputForModel {
                result_text,
                failed: true,
                change_count: 0,
            });
        }

        let ctx = crate::tool::context::ToolContext {
            session: &request.session,
            workspace: &request.session.workspace,
            config: &request.config,
            tool_call_id: record.id,
            cancel: request.cancel.clone(),
            prompt,
            services: &self.tool_services,
        };
        match self.registry.execute(&call.tool_name, arguments, ctx).await {
            Ok(result) => {
                let result_text = tool_result_text(&result);
                let change_count = result.recorded_changes.len();
                let metadata = merge_tool_metadata(metadata, &result);
                if result.change_summaries.is_empty() {
                    let completed = repo
                        .complete_tool_call_with_protocol_bundle(
                            request.session.session.id,
                            assistant_message_id,
                            record.id,
                            record.tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            request.protocol_turn_id,
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?;
                    sink.emit_pre_recorded(completed)?;
                } else {
                    let file_change_evidence = result
                        .change_summaries
                        .iter()
                        .map(|change| crate::protocol::FileChangeEvidence {
                            change_id: change.change_id,
                            kind: change.kind,
                            path_before: change.path_before.clone(),
                            path_after: change.path_after.clone(),
                            summary: change.summary_line(None),
                        })
                        .collect::<Vec<_>>();
                    let diff_summary = crate::session::DiffSummaryPart {
                        tool_call_id: Some(record.id),
                        change_ids: result.recorded_changes.clone(),
                        changes: file_change_evidence,
                        summary: result
                            .change_summaries
                            .iter()
                            .map(|change| change.summary_line(None))
                            .collect::<Vec<_>>()
                            .join("; "),
                    };
                    let (completed, file_changes) = repo
                        .complete_tool_call_with_file_changes_protocol_bundle(
                            request.session.session.id,
                            assistant_message_id,
                            record.id,
                            record.tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            diff_summary,
                            result.change_summaries,
                            request.protocol_turn_id,
                            sink.reserve_protocol_sequence_no(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?;
                    sink.emit_pre_recorded(completed)?;
                    sink.emit_pre_recorded(file_changes)?;
                }
                Ok(ToolOutputForModel {
                    result_text,
                    failed: false,
                    change_count,
                })
            }
            Err(error) => {
                let result_text = error.to_string();
                let failed = repo
                    .fail_tool_call_with_protocol_bundle(
                        request.session.session.id,
                        assistant_message_id,
                        record.id,
                        record.tool_name,
                        &result_text,
                        failed_tool_metadata(metadata),
                        request.protocol_turn_id,
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?;
                sink.emit_pre_recorded(failed)?;
                Ok(ToolOutputForModel {
                    result_text,
                    failed: true,
                    change_count: 0,
                })
            }
        }
    }

    async fn interrupt(
        &self,
        request: &AgentRunRequest,
        assistant_message_id: MessageId,
        usage: Option<TokenUsage>,
        reason: &str,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        let event = RunEvent::SessionInterrupted {
            session_id: request.session.session.id,
            reason: reason.to_string(),
        };
        let metadata = assistant_metadata(request, Some(FinishReason::Cancelled), usage);
        self.store
            .session_repo()
            .update_message_metadata_and_status_with_protocol_event(
                request.session.session.id,
                assistant_message_id,
                &metadata,
                SessionStatus::Cancelled,
                &event,
                request.protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(event)?;
        Ok(RunSummary {
            session_id: request.session.session.id,
            assistant_message_id: Some(assistant_message_id),
            status: SessionStatus::Cancelled,
            finish_reason: Some(FinishReason::Cancelled),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn fail(
        &self,
        request: &AgentRunRequest,
        assistant_message_id: MessageId,
        usage: Option<TokenUsage>,
        message: String,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        let event = RunEvent::SessionFailed {
            session_id: request.session.session.id,
            message,
        };
        let metadata = assistant_metadata(request, Some(FinishReason::Error), usage);
        self.store
            .session_repo()
            .update_message_metadata_and_status_with_protocol_event(
                request.session.session.id,
                assistant_message_id,
                &metadata,
                SessionStatus::Failed,
                &event,
                request.protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(event)?;
        Ok(())
    }

    fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.registry
            .specs()
            .into_iter()
            .map(|spec| ToolSchema {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
                input_schema: spec.input_schema,
                strict: false,
            })
            .collect()
    }
}

struct ToolOutputForModel {
    result_text: String,
    failed: bool,
    change_count: usize,
}

#[derive(Default)]
struct ResponseCollector {
    text: String,
    reasoning: Vec<String>,
    tool_calls: Vec<ModelToolCall>,
    tool_call_args: HashMap<String, String>,
    tool_call_names: HashMap<String, String>,
}

impl LlmEventSink for ResponseCollector {
    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => self.text.push_str(&delta),
            LlmEvent::ReasoningDelta(delta) => self.reasoning.push(delta),
            LlmEvent::ToolCallStart { call_id, tool_name } => {
                self.tool_call_names.insert(call_id.clone(), tool_name);
                self.tool_call_args.entry(call_id).or_default();
            }
            LlmEvent::ToolCallArgsDelta { call_id, delta } => {
                self.tool_call_args
                    .entry(call_id)
                    .or_default()
                    .push_str(&delta);
            }
            LlmEvent::Finished { .. } => {}
        }
        self.rebuild_tool_calls();
        Ok(())
    }
}

impl ResponseCollector {
    fn rebuild_tool_calls(&mut self) {
        let mut calls = self
            .tool_call_names
            .iter()
            .map(|(call_id, tool_name)| ModelToolCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                arguments_json: self
                    .tool_call_args
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| "{}".to_string()),
            })
            .collect::<Vec<_>>();
        calls.sort_by(|left, right| left.call_id.cmp(&right.call_id));
        self.tool_calls = calls;
    }
}

struct LoopGuard {
    max_steps: usize,
    steps: usize,
    repeats: HashMap<String, usize>,
}

impl LoopGuard {
    fn new(max_steps: usize) -> Self {
        Self {
            max_steps: max_steps.max(1),
            steps: 0,
            repeats: HashMap::new(),
        }
    }

    fn check_step_budget(&mut self) -> Result<(), AgentError> {
        if self.steps >= self.max_steps {
            return Err(AgentError::Message(format!(
                "step budget exceeded after {} model request(s)",
                self.max_steps
            )));
        }
        self.steps += 1;
        Ok(())
    }

    fn record_tool_call(&mut self, call: &ModelToolCall) -> Result<(), AgentError> {
        let signature = format!("{}:{}", call.tool_name, call.arguments_json);
        let count = self.repeats.entry(signature.clone()).or_insert(0);
        *count += 1;
        if *count >= 3 {
            return Err(AgentError::Message(format!(
                "repeated identical tool call stopped after {count} attempts: {signature}"
            )));
        }
        Ok(())
    }
}

async fn persist_text_delta(
    repo: &crate::storage::SqliteSessionRepository,
    session_id: crate::session::SessionId,
    message_id: MessageId,
    turn_id: TurnId,
    sink: &mut dyn RunEventSink,
    text: String,
) -> Result<(), AgentError> {
    let event = RunEvent::TextDelta {
        message_id,
        delta: text.clone(),
    };
    repo.append_part_with_protocol_bundle(
        session_id,
        message_id,
        NewPart {
            kind: PartKind::Text,
            payload: MessagePart::Text(TextPart { text }),
        },
        &event,
        turn_id,
        sink.reserve_protocol_sequence_no(),
    )
    .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
}

fn assistant_metadata(
    request: &AgentRunRequest,
    finish_reason: Option<FinishReason>,
    token_usage: Option<TokenUsage>,
) -> MessageMetadata {
    MessageMetadata::Assistant(AssistantMessageMeta {
        model: request.model.name.clone(),
        base_url: request.config.model.base_url.clone(),
        finish_reason,
        token_usage,
        summary: false,
    })
}

fn messages_from_history(history_items: &[HistoryItem]) -> Vec<ModelMessage> {
    let mut ordered = history_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| (item.sequence_no, item.created_at_ms));
    let mut messages = Vec::new();
    let mut tool_names_by_call = HashMap::new();
    for item in ordered {
        match &item.payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::SteerTurn { content, .. } => {
                messages.push(user_message_from_content(content));
            }
            HistoryItemPayload::Message { role, content, .. } => match role {
                MessageRole::User => messages.push(user_message_from_content(content)),
                MessageRole::Assistant => messages.push(ModelMessage::Assistant {
                    content: content_text(content),
                }),
            },
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                effective_arguments,
                model_arguments,
                arguments,
                ..
            } => {
                tool_names_by_call.insert(call_id.to_string(), tool.to_string());
                let selected_arguments = if !effective_arguments.is_null() {
                    effective_arguments
                } else if !model_arguments.is_null() {
                    model_arguments
                } else {
                    arguments
                };
                messages.push(ModelMessage::AssistantToolCalls {
                    content: None,
                    tool_calls: vec![ModelToolCall {
                        call_id: call_id.to_string(),
                        tool_name: tool.to_string(),
                        arguments_json: selected_arguments.to_string(),
                    }],
                });
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                output_text,
                ..
            } => messages.push(ModelMessage::Tool {
                call_id: call_id.to_string(),
                tool_name: tool_names_by_call
                    .get(&call_id.to_string())
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                result: output_text.clone(),
                metadata: Value::Null,
            }),
            HistoryItemPayload::Error { message, .. } => messages.push(ModelMessage::Assistant {
                content: format!("Previous run ended with an error: {message}"),
            }),
            _ => {}
        }
    }
    messages
}

fn user_message_from_content(content: &[ContentPart]) -> ModelMessage {
    let has_image = content
        .iter()
        .any(|part| matches!(part, ContentPart::Image { .. }));
    if !has_image {
        return ModelMessage::User {
            content: content_text(content),
        };
    }
    ModelMessage::UserParts {
        parts: content
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => ModelContentPart::Text { text: text.clone() },
                ContentPart::Image { image } => ModelContentPart::Image {
                    mime_type: image.mime_type.clone(),
                    data_base64: image.data_base64.clone(),
                },
            })
            .collect(),
    }
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_tool_arguments(arguments_json: &str) -> Result<Value, AgentError> {
    serde_json::from_str(arguments_json).map_err(|error| {
        AgentError::Message(format!(
            "invalid tool arguments JSON: {error}; input={arguments_json}"
        ))
    })
}

fn validate_shallow_schema(
    tool_name: &str,
    arguments: Value,
    schemas: &[ToolSchema],
) -> Result<Value, AgentError> {
    let Some(schema) = schemas.iter().find(|schema| schema.name == tool_name) else {
        return Ok(arguments);
    };
    let mut errors = Vec::new();
    validate_json_schema_value(&arguments, &schema.input_schema, "$", &mut errors);
    if errors.is_empty() {
        Ok(arguments)
    } else {
        Err(AgentError::Message(format!(
            "tool `{tool_name}` arguments do not match schema: {}",
            errors.join("; ")
        )))
    }
}

fn validate_json_schema_value(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(expected) = schema.get("type") {
        validate_json_type(value, expected, path, errors);
    }
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        errors.push(format!("{path} is not one of the allowed enum values"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    errors.push(format!("{path}.{key} is required"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, property_schema) in properties {
                if let Some(property_value) = object.get(key) {
                    validate_json_schema_value(
                        property_value,
                        property_schema,
                        &format!("{path}.{key}"),
                        errors,
                    );
                }
            }
        }
    }
    if let Some(items_schema) = schema.get("items")
        && let Some(items) = value.as_array()
    {
        for (index, item) in items.iter().enumerate() {
            validate_json_schema_value(item, items_schema, &format!("{path}[{index}]"), errors);
        }
    }
}

fn validate_json_type(value: &Value, expected: &Value, path: &str, errors: &mut Vec<String>) {
    let matches = match expected {
        Value::String(kind) => json_type_matches(value, kind),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| json_type_matches(value, kind)),
        _ => true,
    };
    if !matches {
        errors.push(format!("{path} expected type {expected}"));
    }
}

fn json_type_matches(value: &Value, kind: &str) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn tool_route_metadata(call: &ModelToolCall, arguments: &Value, schemas: &[ToolSchema]) -> Value {
    serde_json::json!({
        "tool_route": {
            "original_arguments": arguments,
            "effective_arguments": arguments,
            "allowed_tools": schemas.iter().map(|schema| schema.name.clone()).collect::<Vec<_>>()
        },
        "model_call_id": call.call_id,
        "success": true,
        "progress_effect": "made_progress"
    })
}

fn failed_tool_metadata(mut metadata: Value) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert("success".to_string(), Value::Bool(false));
        object.insert(
            "progress_effect".to_string(),
            Value::String("blocked".to_string()),
        );
    }
    metadata
}

fn tool_result_text(result: &ToolResult) -> String {
    if result.output_text.trim().is_empty() {
        result.title.clone()
    } else {
        result.output_text.clone()
    }
}

fn merge_tool_metadata(mut metadata: Value, result: &ToolResult) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert("tool_metadata".to_string(), result.metadata.clone());
        object.insert("success".to_string(), Value::Bool(true));
        object.insert(
            "progress_effect".to_string(),
            Value::String("made_progress".to_string()),
        );
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use camino::Utf8PathBuf;
    use std::sync::Mutex;

    use crate::config::{AccessMode, ResolvedConfig};
    use crate::error::LlmError;
    use crate::llm::{LlmResponseSummary, ModelCapabilities};
    use crate::protocol::{
        ProtocolEventStore, ThreadOp, ToolProgressEffect, UserInputItem, UserTurn,
    };
    use crate::runtime::SystemClock;
    use crate::session::{
        ProjectRepository, PromptDispatchPart, SessionRepository, SessionSelector,
        SessionStartRequest, ToolCallId,
    };
    use crate::storage::{SqliteStore, StoragePaths};
    use crate::tool::ToolName;
    use crate::tool::context::ToolServices;
    use crate::tool::truncate::ToolTruncator;
    use crate::workspace::WorkspaceDiscovery;

    struct ScriptedClient {
        responses: Mutex<Vec<ScriptedResponse>>,
    }

    struct ScriptedResponse {
        events: Vec<LlmEvent>,
        finish_reason: FinishReason,
    }

    #[async_trait(?Send)]
    impl LlmClient for ScriptedClient {
        async fn stream_chat(
            &self,
            _request: ChatRequest,
            _cancel: CancellationToken,
            sink: &mut dyn LlmEventSink,
        ) -> Result<LlmResponseSummary, LlmError> {
            let response = self.responses.lock().expect("responses mutex").remove(0);
            for event in response.events {
                sink.push(event)?;
            }
            Ok(LlmResponseSummary {
                finish_reason: response.finish_reason,
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    reasoning_tokens: None,
                }),
            })
        }
    }

    #[derive(Default)]
    struct CapturingSink {
        events: Vec<RunEvent>,
        sequence_no: i64,
    }

    impl RunEventSink for CapturingSink {
        fn emit(&mut self, event: RunEvent) -> Result<(), crate::error::RuntimeError> {
            self.events.push(event);
            Ok(())
        }

        fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
            let value = self.sequence_no;
            self.sequence_no += 1;
            Some(value)
        }
    }

    struct AllowPrompt;

    impl ConfirmationPrompt for AllowPrompt {
        fn confirm(
            &mut self,
            _request: &crate::tool::PermissionRequest,
        ) -> Result<bool, crate::error::CliPromptError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn thin_loop_runs_scripted_provider_tool_turn() {
        let config = ResolvedConfig::default();
        let run = run_scripted(
            config,
            vec![
                ScriptedResponse {
                    events: vec![
                        LlmEvent::ToolCallStart {
                            call_id: "call_1".to_string(),
                            tool_name: "write".to_string(),
                        },
                        LlmEvent::ToolCallArgsDelta {
                            call_id: "call_1".to_string(),
                            delta: r#"{"path":"hello.txt","content":"hello\n"}"#.to_string(),
                        },
                    ],
                    finish_reason: FinishReason::Stop,
                },
                ScriptedResponse {
                    events: vec![LlmEvent::TextDelta("done".to_string())],
                    finish_reason: FinishReason::Stop,
                },
            ],
        )
        .await
        .expect("run");
        let summary = run.summary.expect("summary");

        assert_eq!(summary.status, SessionStatus::Completed);
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.failed_tool_count, 0);
        assert_eq!(
            std::fs::read_to_string(run.root.join("hello.txt"))
                .expect("written")
                .replace("\r\n", "\n"),
            "hello\n"
        );
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, RunEvent::SessionCompleted { .. }))
        );
        assert!(summary.assistant_message_id.is_some());
    }

    #[tokio::test]
    async fn cancelled_provider_response_terminalizes_cancelled() {
        let config = ResolvedConfig::default();
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: Vec::new(),
                finish_reason: FinishReason::Cancelled,
            }],
        )
        .await
        .expect("run");
        let summary = run.summary.expect("summary");
        let session = run
            .store
            .session_repo()
            .get_session(run.session_id)
            .await
            .expect("session");

        assert_eq!(summary.status, SessionStatus::Cancelled);
        assert_eq!(session.status, SessionStatus::Cancelled);
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, RunEvent::SessionInterrupted { .. }))
        );
    }

    #[tokio::test]
    async fn loop_failure_terminalizes_session_and_assistant_metadata() {
        let mut config = ResolvedConfig::default();
        config.session.max_steps_per_turn = 1;
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::ToolCallStart {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                }],
                finish_reason: FinishReason::Stop,
            }],
        )
        .await
        .expect("run setup");

        assert!(run.summary.is_err());
        let session = run
            .store
            .session_repo()
            .get_session(run.session_id)
            .await
            .expect("session");
        let transcript = run
            .store
            .session_repo()
            .compatibility_transcript(run.session_id)
            .await
            .expect("transcript");
        let assistant = transcript
            .messages
            .iter()
            .find(|message| matches!(message.record.role, MessageRole::Assistant))
            .expect("assistant message");

        assert_eq!(session.status, SessionStatus::Failed);
        assert!(matches!(
            assistant.record.metadata,
            MessageMetadata::Assistant(AssistantMessageMeta {
                finish_reason: Some(FinishReason::Error),
                ..
            })
        ));
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, RunEvent::SessionFailed { .. }))
        );
    }

    #[test]
    fn prompt_asset_stays_small() {
        assert!(include_str!("../../assets/prompts/system.md").len() < 8 * 1024);
    }

    #[test]
    fn history_projection_replays_user_tool_and_output() {
        let call_id = ToolCallId::new();
        let session_id = crate::session::SessionId::new();
        let turn_id = TurnId::new();
        let items = vec![
            HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 0,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "hello".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
            HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 1,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::ToolCall {
                    call_id,
                    tool: ToolName::Read,
                    arguments: serde_json::json!({"path":"README.md"}),
                    model_arguments: Value::Null,
                    effective_arguments: serde_json::json!({"path":"README.md"}),
                    adjusted_arguments: None,
                    permission_decision: None,
                    sandbox_decision: None,
                    allowed_surface: Vec::new(),
                    retry_policy: None,
                    terminal_guard_policy: None,
                },
            },
            HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 2,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::ToolOutput {
                    call_id,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "read".to_string(),
                    output_text: "contents".to_string(),
                    metadata: Value::Null,
                    success: Some(true),
                    progress_effect: ToolProgressEffect::MadeProgress,
                    blocked_action: None,
                    result_hash: None,
                    verification_run: None,
                },
            },
        ];
        let messages = messages_from_history(&items);
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0], ModelMessage::User { .. }));
        assert!(matches!(
            messages[1],
            ModelMessage::AssistantToolCalls { .. }
        ));
        assert!(matches!(messages[2], ModelMessage::Tool { .. }));
    }

    #[test]
    fn error_history_replays_as_assistant_text_not_tool_message() {
        let items = vec![HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: crate::session::SessionId::new(),
            turn_id: TurnId::new(),
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "failed".to_string(),
            },
        }];
        let messages = messages_from_history(&items);

        assert!(matches!(messages[0], ModelMessage::Assistant { .. }));
    }

    #[test]
    fn schema_validation_rejects_required_and_type_mismatches() {
        let schemas = vec![ToolSchema {
            name: "sample".to_string(),
            description: "sample".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path", "count"],
                "properties": {
                    "path": {"type": "string"},
                    "count": {"type": "integer"},
                    "items": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                }
            }),
            strict: false,
        }];

        let missing = validate_shallow_schema("sample", serde_json::json!({"path": 1}), &schemas)
            .expect_err("schema should reject missing and wrong type");
        let ok = validate_shallow_schema(
            "sample",
            serde_json::json!({"path": "a", "count": 1, "items": ["x"]}),
            &schemas,
        );

        assert!(missing.to_string().contains("$.count is required"));
        assert!(missing.to_string().contains("$.path expected type"));
        assert!(ok.is_ok());
    }

    struct ScriptedRun {
        summary: Result<RunSummary, AgentError>,
        store: StoreBundle,
        session_id: crate::session::SessionId,
        events: Vec<RunEvent>,
        root: Utf8PathBuf,
    }

    async fn run_scripted(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
    ) -> Result<ScriptedRun, AgentError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.keep()).expect("utf8 temp");
        let storage_paths = StoragePaths {
            data_dir: root.join(".moyai-data"),
            database_path: root.join(".moyai-data/moyai.sqlite3"),
            truncation_dir: root.join(".moyai-data/truncation"),
        };
        let sqlite = SqliteStore::open(&storage_paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "test", "none")
            .await
            .expect("project");
        let session_service = crate::session::SessionService::new(store.clone());
        let session = session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("test".to_string()),
                    cwd: root.clone(),
                    model: "scripted".to_string(),
                    base_url: "http://local".to_string(),
                    access_mode: AccessMode::FullAccess,
                },
                workspace,
            )
            .await
            .expect("session");
        let session_id = session.session.id;
        let turn_id = TurnId::new();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "write hello.txt".to_string(),
            }],
            prompt_dispatch: Some(PromptDispatchPart::raw("write hello.txt")),
            editor_context: None,
            context: test_turn_context(session.session.id, &root),
        };
        let ThreadOp::UserTurn(user_turn) = ThreadOp::user_turn(user_turn) else {
            unreachable!()
        };
        let user_message = session_service
            .store_user_thread_op_with_protocol_bundle(
                &session,
                &user_turn,
                Some("scripted".to_string()),
                SessionStateSnapshot::default(),
                turn_id,
                0,
            )
            .await
            .expect("user message");
        let runtime_input = RuntimeInputView::from_history_items(
            store
                .protocol_event_store()
                .list_history_items_for_session(session.session.id)
                .expect("history"),
        );
        let tool_services = test_tool_services(&config, &store, storage_paths);
        let registry = ToolRegistry::builtin(tool_services.clone());
        let llm = Arc::new(ScriptedClient {
            responses: Mutex::new(responses),
        });
        let agent = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services);
        let mut sink = CapturingSink {
            events: Vec::new(),
            sequence_no: 1,
        };
        let mut prompt = AllowPrompt;
        let summary = agent
            .run(
                AgentRunRequest {
                    session,
                    user_message_id: user_message.id,
                    protocol_turn_id: turn_id,
                    runtime_input,
                    state: SessionStateSnapshot::default(),
                    config: config.clone(),
                    model: test_model(&config),
                    cancel: CancellationToken::new(),
                },
                &mut prompt,
                &mut sink,
            )
            .await;

        Ok(ScriptedRun {
            summary,
            store,
            session_id,
            events: sink.events,
            root,
        })
    }

    fn test_model(config: &ResolvedConfig) -> ModelProfile {
        ModelProfile {
            name: "scripted".to_string(),
            context_window: 8192,
            max_output_tokens: 1024,
            provider_metadata_mode: config.model.provider_metadata_mode,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        }
    }

    fn test_tool_services(
        config: &ResolvedConfig,
        store: &StoreBundle,
        storage_paths: StoragePaths,
    ) -> ToolServices {
        ToolServices {
            edit_safety: crate::edit::EditSafety::default(),
            formatter: crate::edit::Formatter::new(config.format.clone()),
            change_tracker: crate::edit::ChangeTracker::default(),
            store: store.clone(),
            storage_paths,
            truncator: ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        }
    }

    fn test_turn_context(
        session_id: crate::session::SessionId,
        root: &Utf8PathBuf,
    ) -> crate::protocol::TurnContext {
        crate::protocol::TurnContext {
            session_id,
            cwd: root.clone(),
            workspace_root: root.clone(),
            provider: "scripted".to_string(),
            model: "scripted".to_string(),
            base_url: "http://local".to_string(),
            access_mode: AccessMode::FullAccess,
            sandbox: crate::protocol::SandboxProfile::WorkspaceWrite,
            shell_family: crate::config::ShellFamily::PowerShell,
            model_capabilities: crate::protocol::ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
                parallel_tool_calls: false,
                context_window: 8192,
                max_output_tokens: 1024,
            },
            route: crate::session::TaskRoute::Code,
            process_phase: crate::session::ProcessPhase::Discover,
            active_contract: crate::protocol::ActiveWorkContractProjection {
                route: crate::session::TaskRoute::Code,
                process_phase: crate::session::ProcessPhase::Discover,
                active_work_kind: None,
                summary: "test".to_string(),
                active_targets: Vec::new(),
                operation_intents: Vec::new(),
                required_verification_commands: Vec::new(),
                allowed_tools: Vec::new(),
                forbidden_tools: Vec::new(),
                projection_id: crate::protocol::ProjectionId::new(),
            },
            allowed_tools: Vec::new(),
            tool_choice: crate::protocol::ToolChoice::Auto,
            images: Vec::new(),
            output_contract: crate::protocol::OutputContract {
                final_answer_required: true,
                structured_schema_name: None,
                history_markdown_projection: true,
            },
            continuation: None,
            turn_decision_projection: None,
        }
    }
}

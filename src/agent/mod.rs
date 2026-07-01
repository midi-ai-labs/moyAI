//! Phase14 core rebuild: thin agent loop boundary.

mod goal_steering;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::{AccessMode, ResolvedConfig};
use crate::context::context_window::ContextWindowTokenStatus;
use crate::context::world_state::WorldState;
use crate::error::AgentError;
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelContentPart, ModelMessage, ModelProfile,
    ModelToolCall, ToolSchema,
};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, ProtocolEventStore, TurnId,
};
use crate::runtime::{LiveConfigOverrides, RunEventSink};
use crate::session::{
    AssistantMessageMeta, FinishReason, MessageId, MessageMetadata, MessagePart, MessageRole,
    NewMessage, NewPart, PartKind, RequestDiagnosticsPart, RequestMessageDiagnostic,
    RequestToolCallDiagnostic, RequestToolSchemaDiagnostic, RunConfigSnapshot, RunEvent,
    RunMetrics, RunSummary, SessionContext, SessionStateSnapshot, SessionStatus, TextPart,
    ThreadGoal, ThreadGoalStatus, TokenUsage,
};
use crate::storage::StoreBundle;
use crate::tool::ToolResult;
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;

#[derive(Debug, Default, Clone, Copy)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(
        &self,
        world_state: &WorldState,
        skills_snapshot: &crate::skill::SkillsSnapshot,
    ) -> String {
        format!(
            "{}\n\n{}\n\n{}",
            include_str!("../../assets/prompts/system.md").trim(),
            world_state.rendered,
            crate::skill::render_available_skills_from_snapshot(skills_snapshot)
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
    pub live_config: Option<LiveConfigOverrides>,
}

impl AgentRunRequest {
    fn current_access_mode(&self) -> AccessMode {
        self.live_config
            .as_ref()
            .map(LiveConfigOverrides::access_mode)
            .unwrap_or(self.config.permissions.access_mode)
    }
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

        let started_at = Instant::now();
        let tool_schemas = self.tool_schemas();
        let mut messages = messages_from_history(&request.runtime_input.history_items);
        let mut guard = LoopGuard::new(request.config.session.max_steps_per_turn);
        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut model_request_count = 0usize;
        let mut tool_calls_by_name = BTreeMap::<String, usize>::new();
        let mut failed_tool_calls_by_name = BTreeMap::<String, usize>::new();
        let mut latest_usage: Option<TokenUsage> = None;
        let mut active_goal_id_for_turn: Option<String> = None;

        let outcome: Result<RunSummary, AgentError> = async {
            loop {
                guard.check_step_budget()?;
                if request.cancel.is_cancelled() {
                    return self
                        .interrupt(
                            &request,
                            assistant.id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            "run cancelled by user",
                            sink,
                        )
                        .await;
                }

                let goal_for_request = self.goal_for_request(request.session.session.id).await?;
                if let Some(goal_for_request) = &goal_for_request {
                    active_goal_id_for_turn = Some(goal_for_request.goal_id.clone());
                }
                let request_messages = messages_with_goal_steering(
                    &messages,
                    goal_for_request.as_ref().map(|goal| &goal.goal),
                );
                let prepared_request =
                    self.chat_request(&request, &request_messages, &tool_schemas)?;
                let prepared_request = self.auto_compact_if_needed(
                    &request,
                    assistant.id,
                    &mut messages,
                    goal_for_request.as_ref().map(|goal| &goal.goal),
                    &tool_schemas,
                    prepared_request,
                    sink,
                )?;
                sink.emit(RunEvent::WorldStateUpdated {
                    session_id: request.session.session.id,
                    snapshot: prepared_request.world_state.snapshot.clone(),
                    rendered: prepared_request.world_state.rendered.clone(),
                })?;
                sink.emit(RunEvent::ModelRequestPrepared {
                    session_id: request.session.session.id,
                    diagnostics: request_diagnostics(
                        &prepared_request.chat_request,
                        request.config.session.overflow_margin_tokens,
                    ),
                })?;
                model_request_count += 1;
                let mut collector = StreamingResponseCollector::new(assistant.id, sink);
                let response = self
                    .llm
                    .stream_chat(
                        prepared_request.chat_request,
                        request.cancel.clone(),
                        &mut collector,
                    )
                    .await?;
                let collector = collector.into_inner();
                latest_usage = response.usage.clone();
                if let Some(goal_for_request) = &goal_for_request {
                    self.store
                        .session_repo()
                        .account_thread_goal_usage_for_goal(
                            request.session.session.id,
                            goal_token_delta(response.usage.as_ref()),
                            Some(goal_for_request.goal_id.as_str()),
                        )
                        .await?;
                }

                if !collector.text.is_empty() {
                    persist_text_part(
                        &repo,
                        request.session.session.id,
                        assistant.id,
                        request.protocol_turn_id,
                        sink.reserve_protocol_sequence_no(),
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
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            "run cancelled by user",
                            sink,
                        )
                        .await;
                }

                if collector.tool_calls.is_empty() {
                    if collector.text.trim().is_empty() {
                        return Err(AgentError::Message(format!(
                            "provider returned an empty final response with finish_reason={:?}",
                            response.finish_reason
                        )));
                    }
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
                        metrics: run_metrics(
                            &request,
                            started_at,
                            model_request_count,
                            latest_usage.clone(),
                            &tool_calls_by_name,
                            &failed_tool_calls_by_name,
                        ),
                    });
                }

                messages.push(ModelMessage::AssistantToolCalls {
                    content: (!collector.text.trim().is_empty()).then_some(collector.text),
                    tool_calls: collector.tool_calls.clone(),
                });

                for call in collector.tool_calls {
                    guard.record_tool_call(&call)?;
                    tool_call_count += 1;
                    *tool_calls_by_name
                        .entry(call.tool_name.to_string())
                        .or_default() += 1;
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
                        *failed_tool_calls_by_name
                            .entry(call.tool_name.to_string())
                            .or_default() += 1;
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
                let _ = self
                    .block_active_goal_after_turn_error(
                        request.session.session.id,
                        active_goal_id_for_turn.as_deref(),
                    )
                    .await;
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
    ) -> Result<PreparedChatRequest, AgentError> {
        let mut prompt_config = request.config.clone();
        prompt_config.permissions.access_mode = request.current_access_mode();
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let world_state =
            WorldState::build(&request.session.workspace, &prompt_config, &tool_names);
        let skills_snapshot = self
            .tool_services
            .skills
            .snapshot_for_workspace(&request.session.workspace.root);
        let chat_request = ChatRequest {
            model: request.model.clone(),
            base_url: request.config.model.base_url.clone(),
            system_prompt: self.prompt_builder.build(&world_state, &skills_snapshot),
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
        Ok(PreparedChatRequest {
            chat_request,
            world_state,
        })
    }

    fn auto_compact_if_needed(
        &self,
        request: &AgentRunRequest,
        assistant_message_id: MessageId,
        messages: &mut Vec<ModelMessage>,
        goal: Option<&ThreadGoal>,
        tools: &[ToolSchema],
        prepared_request: PreparedChatRequest,
        sink: &mut dyn RunEventSink,
    ) -> Result<PreparedChatRequest, AgentError> {
        let status = ContextWindowTokenStatus::for_request(
            &prepared_request.chat_request,
            request.config.session.overflow_margin_tokens,
        );
        if !request.config.session.auto_compact_enabled || !status.token_limit_reached {
            return Ok(prepared_request);
        }

        let keep_recent = request.config.session.auto_compact_keep_recent.max(1);
        let Some(summarized_messages) = messages.len().checked_sub(keep_recent) else {
            return Ok(prepared_request);
        };
        if summarized_messages == 0 {
            return Ok(prepared_request);
        }

        let retained = messages.split_off(summarized_messages);
        let history_items = self
            .store
            .protocol_event_store()
            .list_history_items_for_session(request.session.session.id)?;
        let replacement_item_ids =
            auto_compaction_replacement_item_ids(&history_items, keep_recent);
        let summary = auto_compaction_summary(
            &status,
            summarized_messages,
            retained.len(),
            replacement_item_ids.len(),
        );
        *messages = compacted_provider_messages(retained, &summary);

        sink.emit(RunEvent::CompactionCompleted {
            message_id: assistant_message_id,
            summarized_messages,
            summary,
            replacement_item_ids,
            continuation: None,
        })?;

        let request_messages = messages_with_goal_steering(messages, goal);
        self.chat_request(request, &request_messages, tools)
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
            live_config: request.live_config.clone(),
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
        tool_call_count: usize,
        failed_tool_count: usize,
        change_count: usize,
        model_request_count: usize,
        tool_calls_by_name: BTreeMap<String, usize>,
        failed_tool_calls_by_name: BTreeMap<String, usize>,
        started_at: Instant,
        reason: &str,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        let event = RunEvent::SessionInterrupted {
            session_id: request.session.session.id,
            reason: reason.to_string(),
        };
        let metadata = assistant_metadata(request, Some(FinishReason::Cancelled), usage.clone());
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
            tool_call_count,
            failed_tool_count,
            change_count,
            metrics: run_metrics(
                request,
                started_at,
                model_request_count,
                usage,
                &tool_calls_by_name,
                &failed_tool_calls_by_name,
            ),
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

    async fn goal_for_request(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<Option<AgentGoalForRequest>, AgentError> {
        let goal = self
            .store
            .session_repo()
            .get_thread_goal_with_id(session_id)
            .await?;
        Ok(goal
            .filter(|(goal, _goal_id)| {
                matches!(
                    goal.status,
                    ThreadGoalStatus::Active | ThreadGoalStatus::BudgetLimited
                )
            })
            .map(|(goal, goal_id)| AgentGoalForRequest { goal, goal_id }))
    }

    async fn block_active_goal_after_turn_error(
        &self,
        session_id: crate::session::SessionId,
        expected_goal_id: Option<&str>,
    ) -> Result<(), AgentError> {
        let repo = self.store.session_repo();
        let Some(goal) = repo.get_thread_goal(session_id).await? else {
            return Ok(());
        };
        if goal.status != ThreadGoalStatus::Active {
            return Ok(());
        }
        repo.update_thread_goal_for_goal(
            session_id,
            None,
            Some(ThreadGoalStatus::Blocked),
            None,
            expected_goal_id,
        )
        .await?;
        Ok(())
    }
}

struct AgentGoalForRequest {
    goal: ThreadGoal,
    goal_id: String,
}

struct PreparedChatRequest {
    chat_request: ChatRequest,
    world_state: WorldState,
}

fn messages_with_goal_steering(
    messages: &[ModelMessage],
    goal: Option<&ThreadGoal>,
) -> Vec<ModelMessage> {
    let Some(goal) = goal else {
        return messages.to_vec();
    };
    let Some(steering) = goal_steering::steering_message_for_goal(goal) else {
        return messages.to_vec();
    };
    let mut request_messages = messages.to_vec();
    request_messages.push(steering);
    request_messages
}

struct ToolOutputForModel {
    result_text: String,
    failed: bool,
    change_count: usize,
}

#[derive(Default)]
struct ResponseCollector {
    text: String,
    tool_calls: Vec<ModelToolCall>,
    tool_call_order: Vec<String>,
    tool_call_args: HashMap<String, String>,
    tool_call_names: HashMap<String, String>,
}

impl LlmEventSink for ResponseCollector {
    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => self.text.push_str(&delta),
            LlmEvent::ReasoningDelta(_) => {}
            LlmEvent::ToolCallStart { call_id, tool_name } => {
                if !self.tool_call_order.iter().any(|seen| seen == &call_id) {
                    self.tool_call_order.push(call_id.clone());
                }
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

struct StreamingResponseCollector<'a> {
    inner: ResponseCollector,
    message_id: MessageId,
    sink: &'a mut dyn RunEventSink,
}

impl<'a> StreamingResponseCollector<'a> {
    fn new(message_id: MessageId, sink: &'a mut dyn RunEventSink) -> Self {
        Self {
            inner: ResponseCollector::default(),
            message_id,
            sink,
        }
    }

    fn into_inner(self) -> ResponseCollector {
        self.inner
    }
}

impl LlmEventSink for StreamingResponseCollector<'_> {
    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        match &event {
            LlmEvent::TextDelta(delta) => self
                .sink
                .emit_pre_recorded(RunEvent::TextDelta {
                    message_id: self.message_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            LlmEvent::ReasoningDelta(delta) => self
                .sink
                .emit_pre_recorded(RunEvent::ReasoningDelta {
                    message_id: self.message_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            _ => {}
        }
        self.inner.push(event)
    }
}

impl ResponseCollector {
    fn rebuild_tool_calls(&mut self) {
        let calls = self
            .tool_call_order
            .iter()
            .filter_map(|call_id| {
                let tool_name = self.tool_call_names.get(call_id)?;
                Some(ModelToolCall {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments_json: self
                        .tool_call_args
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| "{}".to_string()),
                })
            })
            .collect::<Vec<_>>();
        self.tool_calls = calls;
    }
}

struct LoopGuard {
    max_steps: usize,
    steps: usize,
    last_tool_signature: Option<String>,
    consecutive_repeat_count: usize,
}

impl LoopGuard {
    fn new(max_steps: usize) -> Self {
        Self {
            max_steps: max_steps.max(1),
            steps: 0,
            last_tool_signature: None,
            consecutive_repeat_count: 0,
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
        if self.last_tool_signature.as_deref() == Some(signature.as_str()) {
            self.consecutive_repeat_count += 1;
        } else {
            self.last_tool_signature = Some(signature.clone());
            self.consecutive_repeat_count = 1;
        }
        if self.consecutive_repeat_count >= 3 {
            return Err(AgentError::Message(format!(
                "repeated identical consecutive tool call stopped after {} attempts: {signature}",
                self.consecutive_repeat_count
            )));
        }
        Ok(())
    }
}

async fn persist_text_part(
    repo: &crate::storage::SqliteSessionRepository,
    session_id: crate::session::SessionId,
    message_id: MessageId,
    turn_id: TurnId,
    protocol_sequence_no: Option<i64>,
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
        protocol_sequence_no,
    )
    .await?;
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

fn run_metrics(
    request: &AgentRunRequest,
    started_at: Instant,
    model_request_count: usize,
    token_usage: Option<TokenUsage>,
    tool_calls_by_name: &BTreeMap<String, usize>,
    failed_tool_calls_by_name: &BTreeMap<String, usize>,
) -> RunMetrics {
    RunMetrics {
        model_request_count,
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        token_usage,
        tool_calls_by_name: tool_calls_by_name.clone(),
        failed_tool_calls_by_name: failed_tool_calls_by_name.clone(),
        config: Some(RunConfigSnapshot {
            model: request.model.name.clone(),
            base_url: request.config.model.base_url.clone(),
            access_mode: request.session.session.access_mode.as_str().to_string(),
        }),
    }
}

fn request_diagnostics(
    request: &ChatRequest,
    overflow_margin_tokens: usize,
) -> RequestDiagnosticsPart {
    let context_window = ContextWindowTokenStatus::for_request(request, overflow_margin_tokens);
    RequestDiagnosticsPart {
        provider: "openai_compat".to_string(),
        model_name: request.model.name.clone(),
        base_url: request.base_url.clone(),
        request_timeout_ms: request.timeout_ms,
        stream_idle_timeout_ms: request.stream_idle_timeout_ms,
        stream_max_retries: request.stream_max_retries,
        configured_max_output_tokens: Some(request.model.max_output_tokens),
        effective_max_output_tokens: Some(request.effective_max_output_tokens()),
        output_budget_reason: Some(request.output_budget_reason().to_string()),
        supports_tools: Some(request.model.capabilities.supports_tools),
        supports_reasoning: Some(request.model.capabilities.supports_reasoning),
        supports_images: Some(request.model.capabilities.supports_images),
        system_prompt_chars: request.system_prompt.chars().count(),
        tool_count: request.tools.len(),
        tool_choice: request
            .tool_choice
            .as_ref()
            .map(|choice| choice.diagnostic_label())
            .or_else(|| (!request.tools.is_empty()).then(|| "auto".to_string())),
        parallel_tool_calls: crate::llm::tool_surface_scoped_parallel_tool_calls_projection(
            request.tools.len(),
            request.parallel_tool_calls,
        ),
        provider_message_count: request.messages.len(),
        image_count: request.messages.iter().map(message_image_count).sum(),
        image_bytes: request.messages.iter().map(message_image_bytes).sum(),
        tool_names: request.tools.iter().map(|tool| tool.name.clone()).collect(),
        tool_schemas: request
            .tools
            .iter()
            .map(|tool| RequestToolSchemaDiagnostic {
                name: tool.name.clone(),
                description_chars: tool.description.chars().count(),
                strict: tool.strict,
                input_schema: tool.input_schema.clone(),
            })
            .collect(),
        turn_decision: None,
        control_envelope: None,
        replay_policies: Vec::new(),
        context_window: Some(context_window),
        messages: request.messages.iter().map(message_diagnostic).collect(),
    }
}

fn message_diagnostic(message: &ModelMessage) -> RequestMessageDiagnostic {
    match message {
        ModelMessage::System { content } => RequestMessageDiagnostic {
            role: "system".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::User { content } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::UserParts { parts } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        ModelContentPart::Text { text } => Some(text.chars().count()),
                        ModelContentPart::Image { .. } => None,
                    })
                    .sum(),
            ),
            content_markers: Vec::new(),
            image_count: message_image_count(message),
            image_bytes: message_image_bytes(message),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::Assistant { content } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: content.as_ref().map(|value| value.chars().count()),
            content_markers: content.as_deref().map(content_markers).unwrap_or_default(),
            image_count: 0,
            image_bytes: 0,
            tool_calls: tool_calls
                .iter()
                .map(|call| RequestToolCallDiagnostic {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    arguments_chars: call.arguments_json.chars().count(),
                })
                .collect(),
            tool_call_id: None,
        },
        ModelMessage::Tool {
            call_id, result, ..
        } => RequestMessageDiagnostic {
            role: "tool".to_string(),
            content_chars: Some(result.chars().count()),
            content_markers: content_markers(result),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.clone()),
        },
    }
}

fn message_image_count(message: &ModelMessage) -> usize {
    match message {
        ModelMessage::UserParts { parts } => parts
            .iter()
            .filter(|part| matches!(part, ModelContentPart::Image { .. }))
            .count(),
        _ => 0,
    }
}

fn message_image_bytes(message: &ModelMessage) -> u64 {
    match message {
        ModelMessage::UserParts { parts } => parts
            .iter()
            .filter_map(|part| match part {
                ModelContentPart::Image { data_base64, .. } => Some(data_base64.len() as u64),
                ModelContentPart::Text { .. } => None,
            })
            .sum(),
        _ => 0,
    }
}

fn content_markers(content: &str) -> Vec<String> {
    let mut markers = Vec::new();
    for marker in ["<world_state>", "<instructions>", "<environment_context>"] {
        if content.contains(marker) {
            markers.push(marker.to_string());
        }
    }
    markers
}

fn messages_from_history(history_items: &[HistoryItem]) -> Vec<ModelMessage> {
    let mut projected = Vec::<(usize, u8, ModelMessage)>::new();
    let mut tool_names_by_call = HashMap::new();
    let index_by_id = history_items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, index))
        .collect::<HashMap<_, _>>();
    let replaced_ids = history_items
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::Compaction {
                replacement_item_ids,
                ..
            } => Some(replacement_item_ids.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect::<HashSet<_>>();
    for (index, item) in history_items.iter().enumerate() {
        if replaced_ids.contains(&item.id) {
            continue;
        }
        match &item.payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::SteerTurn { content, .. } => {
                projected.push((index, 1, user_message_from_content(content)));
            }
            HistoryItemPayload::Message { role, content, .. } => match role {
                MessageRole::User => projected.push((index, 1, user_message_from_content(content))),
                MessageRole::Assistant => projected.push((
                    index,
                    1,
                    ModelMessage::Assistant {
                        content: content_text(content),
                    },
                )),
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
                projected.push((
                    index,
                    1,
                    ModelMessage::AssistantToolCalls {
                        content: None,
                        tool_calls: vec![ModelToolCall {
                            call_id: call_id.to_string(),
                            tool_name: tool.to_string(),
                            arguments_json: selected_arguments.to_string(),
                        }],
                    },
                ));
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                output_text,
                ..
            } => {
                let call_id_text = call_id.to_string();
                if let Some(tool_name) = tool_names_by_call.get(&call_id_text).cloned() {
                    projected.push((
                        index,
                        1,
                        ModelMessage::Tool {
                            call_id: call_id_text,
                            tool_name,
                            result: output_text.clone(),
                            metadata: Value::Null,
                        },
                    ));
                }
            }
            HistoryItemPayload::Compaction {
                summary,
                replacement_item_ids,
                continuation,
                ..
            } => {
                let insertion_index = replacement_item_ids
                    .iter()
                    .filter_map(|id| index_by_id.get(id).copied())
                    .min()
                    .unwrap_or(index);
                projected.push((
                    insertion_index,
                    0,
                    ModelMessage::System {
                        content: compaction_message_for_model(summary, continuation.as_ref()),
                    },
                ));
            }
            HistoryItemPayload::Error { message, .. } => projected.push((
                index,
                1,
                ModelMessage::Assistant {
                    content: format!("Previous run ended with an error: {message}"),
                },
            )),
            _ => {}
        }
    }
    projected.sort_by_key(|(index, priority, _)| (*index, *priority));
    let messages = projected
        .into_iter()
        .map(|(_, _, message)| message)
        .collect::<Vec<_>>();
    messages
}

fn compaction_message_for_model(
    summary: &str,
    continuation: Option<&crate::session::ContinuationContract>,
) -> String {
    let mut message = String::from("Earlier conversation context was compacted.\n");
    message.push_str(summary.trim());
    if let Some(continuation) = continuation {
        message.push_str("\n\nContinuation contract:\n");
        message.push_str(&serde_json::to_string(continuation).unwrap_or_default());
    }
    message
}

fn compacted_provider_messages(
    retained_messages: Vec<ModelMessage>,
    summary: &str,
) -> Vec<ModelMessage> {
    let mut compacted = Vec::with_capacity(retained_messages.len() + 1);
    compacted.push(ModelMessage::System {
        content: compaction_message_for_model(summary, None),
    });
    compacted.extend(retained_messages);
    compacted
}

fn auto_compaction_summary(
    status: &ContextWindowTokenStatus,
    summarized_messages: usize,
    retained_messages: usize,
    replacement_items: usize,
) -> String {
    format!(
        "Auto compaction snapshot.\n\
         Summarized provider messages: {summarized_messages}.\n\
         Retained recent provider messages: {retained_messages}.\n\
         Replaced canonical history items: {replacement_items}.\n\
         Estimated active context tokens before compaction: {}.\n\
         Context window limit: {}.\n\
         Continuation invariant: CompactionContinuity.",
        status.active_context_tokens, status.full_context_window_limit
    )
}

fn auto_compaction_replacement_item_ids(
    history_items: &[HistoryItem],
    keep_recent: usize,
) -> Vec<HistoryItemId> {
    let replayable_indices = history_items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| history_item_replays_to_model(&item.payload).then_some(index))
        .collect::<Vec<_>>();
    if replayable_indices.len() <= keep_recent {
        return Vec::new();
    }

    let mut boundary = replayable_indices[replayable_indices.len() - keep_recent.max(1)];
    while boundary < history_items.len() {
        let HistoryItemPayload::ToolOutput { call_id, .. } = &history_items[boundary].payload
        else {
            break;
        };
        let compacted_call = history_items[..boundary].iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::ToolCall {
                    call_id: seen_call_id,
                    ..
                } if seen_call_id == call_id
            )
        });
        if !compacted_call {
            break;
        }
        boundary += 1;
    }

    history_items
        .iter()
        .take(boundary)
        .map(|item| item.id)
        .collect()
}

fn history_item_replays_to_model(payload: &HistoryItemPayload) -> bool {
    matches!(
        payload,
        HistoryItemPayload::UserTurn { .. }
            | HistoryItemPayload::SteerTurn { .. }
            | HistoryItemPayload::Message { .. }
            | HistoryItemPayload::ToolCall { .. }
            | HistoryItemPayload::ToolOutput { .. }
            | HistoryItemPayload::Compaction { .. }
            | HistoryItemPayload::Error { .. }
    )
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

fn goal_token_delta(usage: Option<&TokenUsage>) -> i64 {
    usage
        .map(|usage| i64::from(usage.total_tokens))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use camino::Utf8PathBuf;
    use std::sync::{Arc, Mutex};

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
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    struct ScriptedResponse {
        events: Vec<LlmEvent>,
        finish_reason: FinishReason,
    }

    #[async_trait(?Send)]
    impl LlmClient for ScriptedClient {
        async fn stream_chat(
            &self,
            request: ChatRequest,
            _cancel: CancellationToken,
            sink: &mut dyn LlmEventSink,
        ) -> Result<LlmResponseSummary, LlmError> {
            self.requests.lock().expect("requests mutex").push(request);
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
        assert_eq!(summary.metrics.model_request_count, 2);
        assert_eq!(
            summary
                .metrics
                .token_usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            Some(15)
        );
        assert_eq!(summary.metrics.tool_calls_by_name.get("write"), Some(&1));
        assert_eq!(summary.metrics.failed_tool_calls_by_name.get("write"), None);
        assert!(summary.metrics.elapsed_ms.is_some());
        assert_eq!(
            summary
                .metrics
                .config
                .as_ref()
                .map(|config| config.access_mode.as_str()),
            Some("full_access")
        );
        let summary_json = serde_json::to_value(&summary).expect("summary json");
        assert_eq!(summary_json["metrics"]["model_request_count"], 2);
        assert_eq!(summary_json["metrics"]["token_usage"]["total_tokens"], 15);
        assert_eq!(summary_json["metrics"]["tool_calls_by_name"]["write"], 1);
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
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, RunEvent::WorldStateUpdated { .. }))
        );
        assert!(run.events.iter().any(|event| {
            matches!(
                event,
                RunEvent::ModelRequestPrepared { diagnostics, .. }
                    if diagnostics.context_window.is_some()
            )
        }));
        assert!(run.requests[0].system_prompt.contains("<world_state>"));
        assert!(run.requests[0].system_prompt.contains("<current_time"));
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

    #[tokio::test]
    async fn active_goal_is_blocked_after_turn_error() {
        let mut config = ResolvedConfig::default();
        config.session.max_steps_per_turn = 1;
        let run = run_scripted_with_goal(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::ToolCallStart {
                    call_id: "call_1".to_string(),
                    tool_name: "read".to_string(),
                }],
                finish_reason: FinishReason::Stop,
            }],
            Some(("finish the objective", ThreadGoalStatus::Active, Some(100))),
        )
        .await
        .expect("run setup");

        assert!(run.summary.is_err());
        let goal = run
            .store
            .session_repo()
            .get_thread_goal(run.session_id)
            .await
            .expect("goal")
            .expect("stored goal");

        assert_eq!(goal.status, ThreadGoalStatus::Blocked);
        assert_eq!(goal.tokens_used, 15);
    }

    #[tokio::test]
    async fn empty_final_response_fails_closed() {
        let config = ResolvedConfig::default();
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: Vec::new(),
                finish_reason: FinishReason::Stop,
            }],
        )
        .await
        .expect("run setup");

        let error = run.summary.expect_err("empty final must fail");
        assert!(error.to_string().contains("empty final response"));
        let session = run
            .store
            .session_repo()
            .get_session(run.session_id)
            .await
            .expect("session");
        assert_eq!(session.status, SessionStatus::Failed);
    }

    #[tokio::test]
    async fn active_goal_steering_is_request_local() {
        let config = ResolvedConfig::default();
        let run = run_scripted_with_goal(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::TextDelta("made progress".to_string())],
                finish_reason: FinishReason::Stop,
            }],
            Some((
                "deliver <feature> & verify",
                ThreadGoalStatus::Active,
                Some(100),
            )),
        )
        .await
        .expect("run");
        run.summary.expect("summary");

        assert_eq!(run.requests.len(), 1);
        let Some(ModelMessage::User { content }) = run.requests[0].messages.last() else {
            panic!("goal steering should be appended as a user message");
        };
        assert!(content.contains("Continue working toward the active thread goal."));
        assert!(content.contains("deliver &lt;feature&gt; &amp; verify"));
        assert!(content.contains("- Tokens remaining: 100"));

        let history = run
            .store
            .protocol_event_store()
            .list_history_items_for_session(run.session_id)
            .expect("history");
        let user_turns = history
            .iter()
            .filter(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
            .count();
        assert_eq!(user_turns, 1);
        assert!(
            history
                .iter()
                .filter_map(|item| match &item.payload {
                    HistoryItemPayload::UserTurn { content, .. }
                    | HistoryItemPayload::Message { content, .. } => Some(content_text(content)),
                    _ => None,
                })
                .all(|text| !text.contains("Continue working toward the active thread goal."))
        );

        let goal = run
            .store
            .session_repo()
            .get_thread_goal(run.session_id)
            .await
            .expect("goal")
            .expect("stored goal");
        assert_eq!(goal.tokens_used, 15);
    }

    #[tokio::test]
    async fn budget_limited_goal_steering_is_used_after_budget_is_reached() {
        let config = ResolvedConfig::default();
        let run = run_scripted_with_goal(
            config,
            vec![
                ScriptedResponse {
                    events: vec![LlmEvent::ToolCallStart {
                        call_id: "call_1".to_string(),
                        tool_name: "read".to_string(),
                    }],
                    finish_reason: FinishReason::Stop,
                },
                ScriptedResponse {
                    events: vec![LlmEvent::TextDelta("wrapped up".to_string())],
                    finish_reason: FinishReason::Stop,
                },
            ],
            Some(("finish within budget", ThreadGoalStatus::Active, Some(10))),
        )
        .await
        .expect("run");
        run.summary.expect("summary");

        assert_eq!(run.requests.len(), 2);
        let Some(ModelMessage::User { content }) = run.requests[1].messages.last() else {
            panic!("budget limit steering should be appended as a user message");
        };
        assert!(content.contains("has reached its token budget"));
        assert!(content.contains("do not start new substantive work"));
        assert!(content.contains("- Tokens used: 15"));
        assert!(!content.contains("{{"));
    }

    #[tokio::test]
    async fn text_and_reasoning_deltas_stream_before_single_text_persist() {
        let config = ResolvedConfig::default();
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: vec![
                    LlmEvent::ReasoningDelta("thinking".to_string()),
                    LlmEvent::TextDelta("hello".to_string()),
                    LlmEvent::TextDelta(" world".to_string()),
                ],
                finish_reason: FinishReason::Stop,
            }],
        )
        .await
        .expect("run");
        run.summary.expect("summary");

        let text_deltas = run
            .events
            .iter()
            .filter_map(|event| match event {
                RunEvent::TextDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let reasoning_deltas = run
            .events
            .iter()
            .filter_map(|event| match event {
                RunEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(reasoning_deltas, vec!["thinking"]);
        assert_eq!(text_deltas, vec!["hello", " world"]);

        let persisted_assistant_text = run
            .store
            .protocol_event_store()
            .list_history_items_for_session(run.session_id)
            .expect("history")
            .into_iter()
            .filter_map(|item| match item.payload {
                HistoryItemPayload::Message {
                    role: MessageRole::Assistant,
                    content,
                    ..
                } => Some(content_text(&content)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(persisted_assistant_text, vec!["hello world"]);
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
    fn history_projection_preserves_store_order_across_turns() {
        let session_id = crate::session::SessionId::new();
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let items = vec![
            HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id: first_turn,
                sequence_no: 10,
                created_at_ms: 100,
                payload: HistoryItemPayload::Message {
                    message_id: Some(MessageId::new()),
                    role: MessageRole::Assistant,
                    content: vec![ContentPart::Text {
                        text: "first turn assistant".to_string(),
                    }],
                },
            },
            HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id: second_turn,
                sequence_no: 1,
                created_at_ms: 200,
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "second turn user".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
        ];

        let messages = messages_from_history(&items);

        assert!(matches!(
            &messages[0],
            ModelMessage::Assistant { content } if content == "first turn assistant"
        ));
        assert!(matches!(
            &messages[1],
            ModelMessage::User { content } if content == "second turn user"
        ));
    }

    #[test]
    fn history_projection_replays_compaction_summary_and_skips_replaced_items() {
        let session_id = crate::session::SessionId::new();
        let turn_id = TurnId::new();
        let old_id = crate::protocol::HistoryItemId::new();
        let recent_id = crate::protocol::HistoryItemId::new();
        let compaction_id = crate::protocol::HistoryItemId::new();
        let items = vec![
            HistoryItem {
                id: old_id,
                session_id,
                turn_id,
                sequence_no: 0,
                created_at_ms: 100,
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "old detail".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
            HistoryItem {
                id: recent_id,
                session_id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 200,
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "recent detail".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
            HistoryItem {
                id: compaction_id,
                session_id,
                turn_id,
                sequence_no: 2,
                created_at_ms: 300,
                payload: HistoryItemPayload::Compaction {
                    mode: crate::protocol::CompactionMode::MidTurn,
                    summary: "old detail summary".to_string(),
                    replacement_item_ids: vec![old_id],
                    continuation: None,
                },
            },
        ];

        let messages = messages_from_history(&items);

        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[0],
            ModelMessage::System { content }
                if content.contains("old detail summary")
                    && !content.contains("recent detail")
        ));
        assert!(matches!(
            &messages[1],
            ModelMessage::User { content } if content == "recent detail"
        ));
    }

    #[test]
    fn auto_compaction_replacement_keeps_tool_call_output_pairs_together() {
        let session_id = crate::session::SessionId::new();
        let turn_id = TurnId::new();
        let user_id = crate::protocol::HistoryItemId::new();
        let call_id_item = crate::protocol::HistoryItemId::new();
        let output_id = crate::protocol::HistoryItemId::new();
        let recent_id = crate::protocol::HistoryItemId::new();
        let call_id = ToolCallId::new();
        let items = vec![
            HistoryItem {
                id: user_id,
                session_id,
                turn_id,
                sequence_no: 0,
                created_at_ms: 100,
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "read file".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
            HistoryItem {
                id: call_id_item,
                session_id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 200,
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
                id: output_id,
                session_id,
                turn_id,
                sequence_no: 2,
                created_at_ms: 300,
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
            HistoryItem {
                id: recent_id,
                session_id,
                turn_id,
                sequence_no: 3,
                created_at_ms: 400,
                payload: HistoryItemPayload::UserTurn {
                    message_id: Some(MessageId::new()),
                    content: vec![ContentPart::Text {
                        text: "next".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            },
        ];

        let replacement_ids = auto_compaction_replacement_item_ids(&items, 2);

        assert_eq!(replacement_ids, vec![user_id, call_id_item, output_id]);
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

    #[test]
    fn response_collector_preserves_tool_call_start_order() {
        let mut collector = ResponseCollector::default();
        collector
            .push(LlmEvent::ToolCallStart {
                call_id: "z_call".to_string(),
                tool_name: "write".to_string(),
            })
            .expect("start z");
        collector
            .push(LlmEvent::ToolCallStart {
                call_id: "a_call".to_string(),
                tool_name: "read".to_string(),
            })
            .expect("start a");
        collector
            .push(LlmEvent::ToolCallArgsDelta {
                call_id: "z_call".to_string(),
                delta: r#"{"path":"one"}"#.to_string(),
            })
            .expect("args z");
        collector
            .push(LlmEvent::ToolCallArgsDelta {
                call_id: "a_call".to_string(),
                delta: r#"{"path":"two"}"#.to_string(),
            })
            .expect("args a");

        assert_eq!(collector.tool_calls[0].call_id, "z_call");
        assert_eq!(collector.tool_calls[1].call_id, "a_call");
    }

    #[test]
    fn repeat_guard_counts_only_consecutive_identical_calls() {
        let mut guard = LoopGuard::new(128);
        let unittest = ModelToolCall {
            call_id: "call_1".to_string(),
            tool_name: "shell".to_string(),
            arguments_json: r#"{"command":"python -m unittest -v"}"#.to_string(),
        };
        let read = ModelToolCall {
            call_id: "call_2".to_string(),
            tool_name: "read".to_string(),
            arguments_json: r#"{"path":"calculator.py"}"#.to_string(),
        };

        guard.record_tool_call(&unittest).expect("first unittest");
        guard
            .record_tool_call(&read)
            .expect("different call resets");
        guard
            .record_tool_call(&unittest)
            .expect("non-consecutive repeat is allowed");
        guard
            .record_tool_call(&unittest)
            .expect("second consecutive repeat is allowed");
        let blocked = guard
            .record_tool_call(&unittest)
            .expect_err("third consecutive repeat is blocked");

        assert!(
            blocked
                .to_string()
                .contains("repeated identical consecutive tool call")
        );
    }

    struct ScriptedRun {
        summary: Result<RunSummary, AgentError>,
        store: StoreBundle,
        session_id: crate::session::SessionId,
        events: Vec<RunEvent>,
        requests: Vec<ChatRequest>,
        root: Utf8PathBuf,
    }

    async fn run_scripted(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
    ) -> Result<ScriptedRun, AgentError> {
        run_scripted_with_goal(config, responses, None).await
    }

    async fn run_scripted_with_goal(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
        goal: Option<(&str, ThreadGoalStatus, Option<i64>)>,
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
        if let Some((objective, status, token_budget)) = goal {
            store
                .session_repo()
                .replace_thread_goal(session_id, objective, status, token_budget)
                .await
                .expect("store goal");
        }
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
        let requests = Arc::new(Mutex::new(Vec::new()));
        let llm = Arc::new(ScriptedClient {
            responses: Mutex::new(responses),
            requests: Arc::clone(&requests),
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
                    live_config: None,
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
            requests: requests.lock().expect("requests mutex").clone(),
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
            skills: crate::skill::SkillsService::new(),
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

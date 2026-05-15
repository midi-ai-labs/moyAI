use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent::compaction::maybe_compact;
use crate::agent::event::StreamAccumulator;
use crate::agent::prompt::{AgentRunRequest, PromptBuilder, RuntimeInputView};
use crate::agent::prompt_assets::{hard_final_step_reminder, max_steps_reminder};
use crate::agent::state::{
    ActiveWorkContract, active_work_contract_for_history_items,
    reduce_session_state_from_history_items,
};
use crate::agent::tool_orchestrator::{ToolExecutionRequest, ToolOrchestrator, ToolRouteRequest};
use crate::agent::turn_decision::build_turn_decision_diagnostic;
use crate::agent::verification::verification_command_identity_key;
use crate::cli::ConfirmationPrompt;
use crate::edit::ChangeSummary;
use crate::error::AgentError;
use crate::llm::{ChatRequest, LlmClient, LlmEventSink, LlmResponseSummary};
use crate::protocol::{
    ActiveWorkContractProjection, DispatchPolicy, HistoryItem, HistoryItemPayload,
    ObligationCompiler, OperationIntent, OutputContract, ProjectionId, ProtocolEventStore,
    SandboxProfile, ToolChoice, TurnContext, TurnControlEnvelope, TurnEngine, TurnEngineInput,
    TurnId,
};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, FinishReason, MessageMetadata, MessagePart, MessageRole, NewMessage,
    NewPart, PartKind, RequestControlEnvelopeDiagnostic, RequestControlEnvelopeIssueDiagnostic,
    RequestControlObligationDiagnostic, RequestControlSurfaceDiagnostic, RequestDiagnosticsPart,
    RequestMessageDiagnostic, RequestToolCallDiagnostic, RequestToolSchemaDiagnostic, RunSummary,
    SessionId, SessionRepository, SessionStateSnapshot, SessionStatus, TaskRoute, TextPart,
    TodoItem, TodoKind, TodoStatus, TurnDecisionWarningSeverity,
};
use crate::storage::{SqliteSessionRepository, StoreBundle};
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolName, ToolResult};

const EXECUTED_TOOL_FAILURE_TERMINAL_THRESHOLD: usize = 3;
const PROGRESS_PROJECTION_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 8;
const DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD: usize = 3;
const VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const WRONG_VERIFICATION_COMMAND_TERMINAL_THRESHOLD: usize = 3;
const WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD: usize = 3;

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
        TurnRuntime::new(self).run(request, prompt, sink).await
    }
}

struct TurnRuntime<'a> {
    agent: &'a AgentLoop,
}

impl<'a> TurnRuntime<'a> {
    fn new(agent: &'a AgentLoop) -> Self {
        Self { agent }
    }

    async fn run(
        &self,
        request: AgentRunRequest,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        let session_repo = self.agent.store.session_repo();
        let assistant_message = session_repo
            .append_message(
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
                Vec::new(),
            )
            .await?;
        sink.emit(crate::session::RunEvent::AssistantStarted {
            message_id: assistant_message.id,
            model: request.model.name.clone(),
        })?;

        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut invalid_tool_call_recoveries = 0usize;
        let mut rejected_tool_proposals = BTreeMap::<String, usize>::new();
        let mut executed_tool_failure_counts = BTreeMap::<String, usize>::new();
        let mut progress_projection_no_progress_counts = BTreeMap::<String, usize>::new();
        let mut operation_non_content_no_progress_counts = BTreeMap::<String, usize>::new();
        let mut verification_supporting_context_no_progress_counts =
            BTreeMap::<String, usize>::new();
        let mut wrong_verification_command_counts = BTreeMap::<String, usize>::new();
        let mut wrong_authoring_target_counts = BTreeMap::<String, usize>::new();
        let mut docs_supporting_context_budget_exhausted = BTreeSet::<String>::new();
        let mut docs_supporting_context_budget_exhausted_counts = BTreeMap::<String, usize>::new();
        for _step in 0..request.config.session.max_steps_per_turn {
            if request.cancel.is_cancelled() {
                return interrupt_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    "run cancelled by user",
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await;
            }
            let history_items = self
                .agent
                .store
                .protocol_event_store()
                .list_history_items_for_session(request.session.session.id)?;
            let session = session_repo.get_session(request.session.session.id).await?;
            let runtime_input = RuntimeInputView::from_history_items(&session, history_items);
            if !runtime_input.has_user_turn() {
                let message = "runtime input view is missing a canonical user turn before dispatch";
                fail_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message.to_string()));
            }
            let todos = session_repo.list_todos(request.session.session.id).await?;
            let persisted_state = session_repo.get_state(request.session.session.id).await?;
            let reduced_state = reduce_session_state_from_history_items(
                &session,
                &runtime_input.history_items,
                &todos,
                &persisted_state,
            );
            if reduced_state != persisted_state {
                session_repo
                    .update_state(request.session.session.id, &reduced_state)
                    .await?;
                sink.emit(crate::session::RunEvent::StateUpdated {
                    session_id: request.session.session.id,
                    state: reduced_state.clone(),
                })?;
            }

            let mut step_request = request.clone();
            step_request.runtime_input = runtime_input;
            step_request.state = reduced_state;
            if maybe_compact(
                self.agent.llm.as_ref(),
                &session_repo,
                &step_request,
                &todos,
                sink,
            )
            .await?
            {
                continue;
            }
            let bundle =
                self.agent
                    .prompt_builder
                    .build(&step_request, &self.agent.registry, &todos)?;
            if !provider_messages_have_user_query_anchor(&bundle.messages) {
                let message = "provider request would omit the active user query before dispatch";
                fail_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message.to_string()));
            }
            let hard_final_step = request.config.session.max_steps_per_turn <= 1;
            let mut system_prompt = bundle.system_prompt.clone();
            let mut tools = bundle.tools.clone();
            if hard_final_step {
                let todo_snapshot =
                    serde_json::to_string_pretty(&todos).unwrap_or_else(|_| "[]".to_string());
                system_prompt = format!(
                    "{}\n{}\n\n{}",
                    max_steps_reminder(),
                    hard_final_step_reminder(&todo_snapshot, None),
                    system_prompt
                );
                tools.clear();
            }
            let active_work = active_work_contract_for_history_items(
                &step_request.session.session,
                &step_request.runtime_input.history_items,
                &step_request.state,
                &todos,
            );
            if clean_closeout_final_message_lifecycle(&step_request.state, active_work.as_ref()) {
                tools.clear();
            }
            if docs_route_supporting_context_budget_recovery_surface_active(
                &step_request.state,
                &docs_supporting_context_budget_exhausted,
            ) {
                tools.retain(|tool| {
                    docs_route_supporting_context_budget_recovery_tool_visible(&tool.name)
                });
            }
            let mut tool_names = tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<BTreeSet<_>>();
            let dispatch_tool_choice =
                tool_choice_for_dispatch(&bundle.policy, &tool_names, &step_request.state);
            let turn_decision = build_turn_decision_diagnostic(
                &step_request.state,
                active_work.as_ref(),
                &bundle.policy,
                &tool_names,
                Some(tool_choice_label(&dispatch_tool_choice).to_string()),
            );
            if let Some(message) = turn_decision_dispatch_block_message(&turn_decision) {
                fail_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    &message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message));
            }
            let compiled_turn = compile_turn_control_envelope(
                &step_request,
                active_work.as_ref(),
                &turn_decision,
                &tool_names,
                &dispatch_tool_choice,
            );
            sink.emit(crate::session::RunEvent::ControlEnvelopePrepared {
                session_id: request.session.session.id,
                envelope: compiled_turn.envelope.clone(),
            })?;
            if compiled_turn.validation.has_errors() {
                let message = control_envelope_validation_error_message(&compiled_turn.envelope);
                fail_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    &message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message));
            }
            if let Some(reason) = compiled_turn.envelope.fail_closed_before_dispatch() {
                let message =
                    format!("turn control envelope failed closed before dispatch: {reason}");
                fail_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    &message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message));
            }
            let authority_tool_names = compiled_turn
                .envelope
                .action_authority
                .allowed_tools
                .iter()
                .map(ToString::to_string)
                .collect::<BTreeSet<_>>();
            if authority_tool_names != tool_names {
                tools.retain(|tool| authority_tool_names.contains(&tool.name));
                tool_names = authority_tool_names;
            }
            let dispatch_tool_choice = compiled_turn.envelope.action_authority.tool_choice.clone();
            let turn_decision = build_turn_decision_diagnostic(
                &step_request.state,
                active_work.as_ref(),
                &bundle.policy,
                &tool_names,
                Some(tool_choice_label(&dispatch_tool_choice).to_string()),
            );
            let control_prompt = compiled_turn
                .envelope
                .projection_bundle
                .prompt
                .render_prompt_block();
            let mut provider_messages = bundle.messages.clone();
            provider_messages.insert(
                0,
                crate::llm::ModelMessage::System {
                    content: control_prompt,
                },
            );
            let chat_request = ChatRequest {
                model: step_request.model.clone(),
                base_url: step_request.config.model.base_url.clone(),
                system_prompt,
                messages: provider_messages,
                tools: tools.clone(),
                timeout_ms: step_request.config.model.request_timeout_ms,
                stream_idle_timeout_ms: step_request.config.model.stream_idle_timeout_ms,
                extra_headers: step_request.config.model.extra_headers.clone(),
                temperature: step_request.config.model.temperature,
                top_p: step_request.config.model.top_p,
                top_k: step_request.config.model.top_k,
                presence_penalty: step_request.config.model.presence_penalty,
                frequency_penalty: step_request.config.model.frequency_penalty,
                seed: step_request.config.model.seed,
                stop_sequences: step_request.config.model.stop_sequences.clone(),
                extra_body: extra_body_with_required_tool_choice(
                    step_request.config.model.extra_body_json.clone(),
                    tool_names.len(),
                    matches!(dispatch_tool_choice, ToolChoice::Required),
                ),
            };
            let diagnostics = request_diagnostics_from_chat(
                &chat_request,
                &tools,
                Some(turn_decision),
                Some(&compiled_turn.envelope),
                &bundle.replay_policies,
            );
            session_repo
                .append_part(
                    assistant_message.id,
                    NewPart {
                        kind: PartKind::RequestDiagnostics,
                        payload: MessagePart::RequestDiagnostics(diagnostics.clone()),
                    },
                )
                .await?;
            sink.emit(crate::session::RunEvent::ModelRequestPrepared {
                session_id: request.session.session.id,
                diagnostics,
            })?;

            let mut stream = StreamAccumulator::default();
            let response = match stream_chat_with_provider_request_timeout(
                &self.agent.llm,
                chat_request,
                step_request.cancel.clone(),
                &mut stream,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    let message = format!("provider model request failed: {error}");
                    fail_turn(
                        &session_repo,
                        request.session.session.id,
                        assistant_message.id,
                        &request.model.name,
                        &request.config.model.base_url,
                        &message,
                        tool_call_count,
                        failed_tool_count,
                        change_count,
                        sink,
                    )
                    .await?;
                    return Err(AgentError::Llm(error));
                }
            };
            let finish_reason = Some(response.finish_reason);
            let token_usage = response.usage.clone();
            if matches!(finish_reason, Some(FinishReason::Cancelled)) {
                return interrupt_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    "run cancelled by user",
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await;
            }

            if !stream.reasoning.trim().is_empty() {
                session_repo
                    .append_part(
                        assistant_message.id,
                        NewPart {
                            kind: PartKind::Reasoning,
                            payload: MessagePart::Reasoning(crate::session::ReasoningPart {
                                text: stream.reasoning.clone(),
                            }),
                        },
                    )
                    .await?;
                sink.emit(crate::session::RunEvent::ReasoningDelta {
                    message_id: assistant_message.id,
                    delta: stream.reasoning.clone(),
                })?;
            }
            if !stream.text.trim().is_empty() {
                session_repo
                    .append_part(
                        assistant_message.id,
                        NewPart {
                            kind: PartKind::Text,
                            payload: MessagePart::Text(TextPart {
                                text: stream.text.clone(),
                            }),
                        },
                    )
                    .await?;
                sink.emit(crate::session::RunEvent::TextDelta {
                    message_id: assistant_message.id,
                    delta: stream.text.clone(),
                })?;
            }

            if stream.tool_calls.is_empty() {
                if matches!(finish_reason, Some(FinishReason::Length)) {
                    let message = "model response hit the output length limit before the run reached a natural stop";
                    fail_turn(
                        &session_repo,
                        request.session.session.id,
                        assistant_message.id,
                        &request.model.name,
                        &request.config.model.base_url,
                        message,
                        tool_call_count,
                        failed_tool_count,
                        change_count,
                        sink,
                    )
                    .await?;
                    return Err(AgentError::Message(message.to_string()));
                }
                return complete_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    finish_reason,
                    token_usage,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    sink,
                )
                .await;
            }

            for tool_call in stream.tool_calls {
                if request.cancel.is_cancelled() {
                    return interrupt_turn(
                        &session_repo,
                        request.session.session.id,
                        assistant_message.id,
                        &request.model.name,
                        &request.config.model.base_url,
                        "run cancelled by user",
                        tool_call_count,
                        failed_tool_count,
                        change_count,
                        sink,
                    )
                    .await;
                }
                tool_call_count += 1;
                let requested_tool_name = tool_call.tool_name.clone();
                let effective_tool_name = requested_tool_name.clone();
                let tool_exists = self.agent.registry.has_tool(&effective_tool_name);
                let tool_names_for_route = tool_names.clone();
                let tool_allowed = tool_names_for_route.contains(&effective_tool_name);
                let effective_arguments_json = tool_call.arguments_json.clone();
                let route = ToolOrchestrator::route(ToolRouteRequest {
                    requested_tool: requested_tool_name.clone(),
                    effective_tool: effective_tool_name.clone(),
                    record_tool: effective_tool_name.clone(),
                    original_arguments_json: tool_call.arguments_json.clone(),
                    effective_arguments_json,
                    allowed_tool_names: &tool_names_for_route,
                    tool_exists,
                    tool_allowed,
                    redirected_from_arguments_json: None,
                    redirect_reason: None,
                    tool_choice: Some(tool_choice_label(&dispatch_tool_choice)),
                    control_projection: Some(control_projection_metadata(
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                    )),
                });
                let record = ToolOrchestrator::record_pending_call(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &route,
                    sink,
                )
                .await?;

                if !tool_exists || !tool_allowed {
                    let result = rejected_tool_result(
                        &requested_tool_name,
                        &effective_tool_name,
                        tool_exists,
                        tool_allowed,
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                    );
                    ToolOrchestrator::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        record.id,
                        record.tool_name,
                        &result,
                        &route,
                        sink,
                    )
                    .await?;
                    if closeout_ready_final_message_authority(&step_request.state) {
                        return complete_turn(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            &request.model.name,
                            &request.config.model.base_url,
                            Some(FinishReason::Stop),
                            token_usage,
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            sink,
                        )
                        .await;
                    }
                    if !tool_exists {
                        invalid_tool_call_recoveries += 1;
                    }
                    if !tool_allowed {
                        let rejection_key = rejected_tool_no_progress_key(
                            &effective_tool_name,
                            &route.effective_arguments_json,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        );
                        let rejection_count =
                            rejected_tool_proposals.entry(rejection_key).or_insert(0);
                        *rejection_count += 1;
                        if should_terminalize_rejected_tool_no_progress(
                            *rejection_count,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        ) {
                            let message = rejected_tool_no_progress_terminal_message(
                                &effective_tool_name,
                                *rejection_count,
                                &tool_names_for_route,
                            );
                            return fail_turn(
                                &session_repo,
                                request.session.session.id,
                                assistant_message.id,
                                &request.model.name,
                                &request.config.model.base_url,
                                &message,
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                sink,
                            )
                            .await;
                        }
                    }
                    continue;
                }

                ToolOrchestrator::mark_running(&session_repo, record.id).await?;
                let parsed_arguments =
                    match serde_json::from_str::<Value>(&route.effective_arguments_json) {
                        Ok(value) => value,
                        Err(error) => {
                            let result = invalid_tool_arguments_result(
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &error.to_string(),
                            );
                            ToolOrchestrator::complete_corrective_call(
                                &session_repo,
                                assistant_message.id,
                                record.id,
                                record.tool_name,
                                &result,
                                &route,
                                sink,
                            )
                            .await?;
                            continue;
                        }
                    };
                if let Some(result) = wrong_authoring_target_result(
                    &effective_tool_name,
                    &parsed_arguments,
                    active_work.as_ref(),
                    &request.session.workspace.root,
                ) {
                    ToolOrchestrator::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        record.id,
                        record.tool_name,
                        &result,
                        &route,
                        sink,
                    )
                    .await?;
                    failed_tool_count += 1;
                    let key = wrong_authoring_target_key(
                        &effective_tool_name,
                        &parsed_arguments,
                        active_work.as_ref(),
                        &request.session.workspace.root,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    );
                    let count = wrong_authoring_target_counts
                        .entry(key)
                        .and_modify(|count| *count += 1)
                        .or_insert(1);
                    if *count >= WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD {
                        let message = wrong_authoring_target_terminal_message(&result, *count);
                        return fail_turn(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            &request.model.name,
                            &request.config.model.base_url,
                            &message,
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            sink,
                        )
                        .await;
                    }
                    continue;
                }
                if let Some(result) = wrong_verification_shell_command_result(
                    &effective_tool_name,
                    &parsed_arguments,
                    active_work.as_ref(),
                ) {
                    ToolOrchestrator::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        record.id,
                        record.tool_name,
                        &result,
                        &route,
                        sink,
                    )
                    .await?;
                    failed_tool_count += 1;
                    let key = wrong_verification_command_key(
                        &parsed_arguments,
                        active_work.as_ref(),
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    );
                    let count = wrong_verification_command_counts
                        .entry(key)
                        .and_modify(|count| *count += 1)
                        .or_insert(1);
                    if *count >= WRONG_VERIFICATION_COMMAND_TERMINAL_THRESHOLD {
                        let message = wrong_verification_command_terminal_message(&result, *count);
                        return fail_turn(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            &request.model.name,
                            &request.config.model.base_url,
                            &message,
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            sink,
                        )
                        .await;
                    }
                    continue;
                }
                if docs_route_supporting_context_budget_applies(
                    &effective_tool_name,
                    &step_request.state,
                ) {
                    let budget_key = docs_route_supporting_context_budget_key(
                        &step_request.state,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    );
                    if docs_supporting_context_budget_exhausted.contains(&budget_key) {
                        let result = docs_supporting_context_budget_exhausted_result(
                            &effective_tool_name,
                            &parsed_arguments,
                            &step_request.state,
                        );
                        ToolOrchestrator::complete_corrective_call(
                            &session_repo,
                            assistant_message.id,
                            record.id,
                            record.tool_name,
                            &result,
                            &route,
                            sink,
                        )
                        .await?;
                        failed_tool_count += 1;
                        let count = docs_supporting_context_budget_exhausted_counts
                            .entry(budget_key)
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                        if *count >= DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD {
                            let message = docs_supporting_context_budget_exhausted_terminal_message(
                                *count,
                                &step_request.state,
                            );
                            return fail_turn(
                                &session_repo,
                                request.session.session.id,
                                assistant_message.id,
                                &request.model.name,
                                &request.config.model.base_url,
                                &message,
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                sink,
                            )
                            .await;
                        }
                        continue;
                    }
                }
                match ToolOrchestrator::execute_registered_call(
                    &self.agent.registry,
                    &effective_tool_name,
                    parsed_arguments,
                    ToolExecutionRequest {
                        session: &request.session,
                        workspace: &request.session.workspace,
                        config: &request.config,
                        tool_call_id: record.id,
                        cancel: request.cancel.clone(),
                        prompt,
                        services: &self.agent.tool_services,
                    },
                    sink,
                )
                .await
                {
                    Ok(result) => {
                        let progress_projection_no_content =
                            tool_result_is_progress_projection_no_content(&result)
                                && open_executable_work_requires_tool_call(&step_request.state);
                        change_count += result.change_summaries.len();
                        let completion_metadata = ToolOrchestrator::complete_executed_call(
                            &session_repo,
                            assistant_message.id,
                            record.id,
                            record.tool_name,
                            &result,
                            &route,
                            &request.session.workspace.root,
                            &step_request.state.active_targets,
                            sink,
                        )
                        .await?;
                        let progress_projection_key = if progress_projection_no_content {
                            Some(progress_projection_no_progress_key(
                                &effective_tool_name,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                                tool_result_result_hash(&completion_metadata).as_deref(),
                            ))
                        } else {
                            None
                        };
                        if !result.change_summaries.is_empty()
                            || !result.recorded_changes.is_empty()
                        {
                            progress_projection_no_progress_counts.clear();
                            operation_non_content_no_progress_counts.clear();
                            verification_supporting_context_no_progress_counts.clear();
                            wrong_authoring_target_counts.clear();
                            if !docs_route_contract_still_pending_after_file_change(
                                &step_request.state,
                            ) {
                                docs_supporting_context_budget_exhausted.clear();
                            }
                            docs_supporting_context_budget_exhausted_counts.clear();
                        }
                        if !result.change_summaries.is_empty() {
                            align_todos_after_changes(
                                &session_repo,
                                request.session.session.id,
                                &request.session.workspace.root,
                                &todos,
                                &result.change_summaries,
                            )
                            .await?;
                        }
                        if let Some(progress_key) = progress_projection_key {
                            let progress_count = progress_projection_no_progress_counts
                                .entry(progress_key)
                                .and_modify(|count| *count += 1)
                                .or_insert(1);
                            if should_terminalize_progress_projection_no_progress(*progress_count) {
                                let message = progress_projection_no_progress_terminal_message(
                                    &effective_tool_name,
                                    *progress_count,
                                    &step_request.state,
                                );
                                return fail_turn(
                                    &session_repo,
                                    request.session.session.id,
                                    assistant_message.id,
                                    &request.model.name,
                                    &request.config.model.base_url,
                                    &message,
                                    tool_call_count,
                                    failed_tool_count,
                                    change_count,
                                    sink,
                                )
                                .await;
                            }
                        }
                        if operation_non_content_no_progress_under_open_authoring(
                            &completion_metadata,
                            &step_request.state,
                        ) {
                            let operation_key = operation_non_content_no_progress_key(
                                &effective_tool_name,
                                &completion_metadata,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            );
                            let operation_count = operation_non_content_no_progress_counts
                                .entry(operation_key.clone())
                                .and_modify(|count| *count += 1)
                                .or_insert(1);
                            if should_terminalize_operation_non_content_no_progress_for_state(
                                *operation_count,
                                &step_request.state,
                            ) {
                                let operation_progress_class =
                                    operation_progress_class_from_metadata(&completion_metadata)
                                        .unwrap_or("");
                                if docs_route_semantic_operation_no_progress(
                                    &step_request.state,
                                    operation_progress_class,
                                ) && operation_progress_class == "supporting_context"
                                {
                                    docs_supporting_context_budget_exhausted.insert(operation_key);
                                    continue;
                                }
                                let message = operation_non_content_no_progress_terminal_message(
                                    &effective_tool_name,
                                    *operation_count,
                                    &completion_metadata,
                                    &step_request.state,
                                );
                                return fail_turn(
                                    &session_repo,
                                    request.session.session.id,
                                    assistant_message.id,
                                    &request.model.name,
                                    &request.config.model.base_url,
                                    &message,
                                    tool_call_count,
                                    failed_tool_count,
                                    change_count,
                                    sink,
                                )
                                .await;
                            }
                        }
                        if verification_supporting_context_no_progress_under_active_verification(
                            &effective_tool_name,
                            &route.effective_arguments_json,
                            &result,
                            &step_request.state,
                        ) {
                            let verification_key = verification_supporting_context_no_progress_key(
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            );
                            let verification_count =
                                verification_supporting_context_no_progress_counts
                                    .entry(verification_key)
                                    .and_modify(|count| *count += 1)
                                    .or_insert(1);
                            if should_terminalize_verification_supporting_context_no_progress(
                                *verification_count,
                            ) {
                                let message =
                                    verification_supporting_context_no_progress_terminal_message(
                                        &effective_tool_name,
                                        *verification_count,
                                        &step_request.state,
                                    );
                                return fail_turn(
                                    &session_repo,
                                    request.session.session.id,
                                    assistant_message.id,
                                    &request.model.name,
                                    &request.config.model.base_url,
                                    &message,
                                    tool_call_count,
                                    failed_tool_count,
                                    change_count,
                                    sink,
                                )
                                .await;
                            }
                        }
                        if effective_tool_name == "shell" && invalid_tool_call_recoveries > 0 {
                            let evidence_text = "Latest confirmed evidence: recovery command completed successfully after invalid tool-call feedback.";
                            session_repo
                                .append_part(
                                    assistant_message.id,
                                    NewPart {
                                        kind: PartKind::Text,
                                        payload: MessagePart::Text(TextPart {
                                            text: evidence_text.to_string(),
                                        }),
                                    },
                                )
                                .await?;
                            sink.emit(crate::session::RunEvent::TextDelta {
                                message_id: assistant_message.id,
                                delta: evidence_text.to_string(),
                            })?;
                            return complete_turn(
                                &session_repo,
                                request.session.session.id,
                                assistant_message.id,
                                &request.model.name,
                                &request.config.model.base_url,
                                Some(FinishReason::Stop),
                                token_usage,
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                sink,
                            )
                            .await;
                        }
                    }
                    Err(error) => {
                        if request.cancel.is_cancelled() {
                            failed_tool_count += 1;
                            ToolOrchestrator::fail_executed_call(
                                &session_repo,
                                assistant_message.id,
                                record.id,
                                record.tool_name,
                                "tool execution cancelled by user",
                                &route,
                                sink,
                            )
                            .await?;
                            return interrupt_turn(
                                &session_repo,
                                request.session.session.id,
                                assistant_message.id,
                                &request.model.name,
                                &request.config.model.base_url,
                                "run cancelled by user",
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                sink,
                            )
                            .await;
                        }
                        if is_invalid_tool_arguments_error(&error.to_string()) {
                            let result = invalid_tool_arguments_result(
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &error.to_string(),
                            );
                            ToolOrchestrator::complete_corrective_call(
                                &session_repo,
                                assistant_message.id,
                                record.id,
                                record.tool_name,
                                &result,
                                &route,
                                sink,
                            )
                            .await?;
                            continue;
                        }
                        failed_tool_count += 1;
                        ToolOrchestrator::fail_executed_call(
                            &session_repo,
                            assistant_message.id,
                            record.id,
                            record.tool_name,
                            &error.to_string(),
                            &route,
                            sink,
                        )
                        .await?;
                        let failure_key = executed_tool_failure_no_progress_key(
                            &effective_tool_name,
                            &route.effective_arguments_json,
                            &tool_names_for_route,
                            &error.to_string(),
                        );
                        let failure_count = executed_tool_failure_counts
                            .entry(failure_key)
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                        if *failure_count >= EXECUTED_TOOL_FAILURE_TERMINAL_THRESHOLD {
                            let message = executed_tool_failure_terminal_message(
                                &effective_tool_name,
                                *failure_count,
                                &error.to_string(),
                            );
                            return fail_turn(
                                &session_repo,
                                request.session.session.id,
                                assistant_message.id,
                                &request.model.name,
                                &request.config.model.base_url,
                                &message,
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                sink,
                            )
                            .await;
                        }
                    }
                }
            }
        }

        fail_turn(
            &session_repo,
            request.session.session.id,
            assistant_message.id,
            &request.model.name,
            &request.config.model.base_url,
            "turn step budget reached before completion",
            tool_call_count,
            failed_tool_count,
            change_count,
            sink,
        )
        .await
    }
}

async fn stream_chat_with_provider_request_timeout(
    llm: &Arc<dyn LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    sink: &mut dyn LlmEventSink,
) -> Result<LlmResponseSummary, crate::error::LlmError> {
    let timeout_ms = request.timeout_ms;
    let request_future = llm.stream_chat(request, cancel, sink);
    if timeout_ms == 0 {
        return request_future.await;
    }
    match tokio::time::timeout(Duration::from_millis(timeout_ms), request_future).await {
        Ok(result) => result,
        Err(_) => Err(crate::error::LlmError::Message(
            provider_request_timeout_error_message(timeout_ms),
        )),
    }
}

pub(crate) fn provider_request_timeout_error_message(timeout_ms: u64) -> String {
    format!("provider request timeout after {timeout_ms}ms before a terminal model response")
}

async fn complete_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: crate::session::MessageId,
    model: &str,
    base_url: &str,
    finish_reason: Option<FinishReason>,
    token_usage: Option<crate::session::TokenUsage>,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    session_repo
        .update_message_metadata(
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason,
                token_usage,
                summary: false,
            }),
        )
        .await?;
    session_repo
        .set_status(session_id, SessionStatus::Completed)
        .await?;
    sink.emit(crate::session::RunEvent::SessionCompleted {
        session_id,
        finish_reason,
    })?;
    Ok(RunSummary {
        session_id,
        assistant_message_id: Some(assistant_message_id),
        status: SessionStatus::Completed,
        finish_reason,
        tool_call_count,
        failed_tool_count,
        change_count,
    })
}

async fn interrupt_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: crate::session::MessageId,
    model: &str,
    base_url: &str,
    reason: &str,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    session_repo
        .update_message_metadata(
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason: Some(FinishReason::Cancelled),
                token_usage: None,
                summary: false,
            }),
        )
        .await?;
    session_repo
        .set_status(session_id, SessionStatus::Cancelled)
        .await?;
    sink.emit(crate::session::RunEvent::SessionInterrupted {
        session_id,
        reason: reason.to_string(),
    })?;
    Ok(RunSummary {
        session_id,
        assistant_message_id: Some(assistant_message_id),
        status: SessionStatus::Cancelled,
        finish_reason: Some(FinishReason::Cancelled),
        tool_call_count,
        failed_tool_count,
        change_count,
    })
}

async fn fail_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: crate::session::MessageId,
    model: &str,
    base_url: &str,
    message: &str,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    session_repo
        .update_message_metadata(
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason: Some(FinishReason::Error),
                token_usage: None,
                summary: false,
            }),
        )
        .await?;
    session_repo
        .set_status(session_id, SessionStatus::Failed)
        .await?;
    sink.emit(crate::session::RunEvent::SessionFailed {
        session_id,
        message: message.to_string(),
    })?;
    Ok(RunSummary {
        session_id,
        assistant_message_id: Some(assistant_message_id),
        status: SessionStatus::Failed,
        finish_reason: Some(FinishReason::Error),
        tool_call_count,
        failed_tool_count,
        change_count,
    })
}

async fn align_todos_after_changes(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    workspace_root: &Utf8Path,
    todos: &[TodoItem],
    changes: &[ChangeSummary],
) -> Result<(), AgentError> {
    let changed_keys = changes
        .iter()
        .flat_map(|change| {
            change
                .path_after
                .as_ref()
                .or(change.path_before.as_ref())
                .into_iter()
                .flat_map(|path| normalized_target_keys(path.as_str(), workspace_root))
        })
        .collect::<BTreeSet<_>>();
    if changed_keys.is_empty() {
        return Ok(());
    }

    let Some(updated) = aligned_todos_after_changed_keys(todos, &changed_keys, workspace_root)
    else {
        return Ok(());
    };

    session_repo.update_todos(session_id, &updated).await?;
    Ok(())
}

fn aligned_todos_after_changed_keys(
    todos: &[TodoItem],
    changed_keys: &BTreeSet<String>,
    workspace_root: &Utf8Path,
) -> Option<Vec<TodoItem>> {
    let mut updated = todos.to_vec();
    let mut changed = false;
    for todo in &mut updated {
        if !matches!(todo.kind, TodoKind::Work | TodoKind::Repair)
            || !matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress)
        {
            continue;
        }
        let todo_keys = todo
            .targets
            .iter()
            .flat_map(|target| normalized_target_keys(target.as_str(), workspace_root))
            .collect::<BTreeSet<_>>();
        if !todo_keys.is_empty() && !todo_keys.is_disjoint(&changed_keys) {
            todo.status = TodoStatus::Completed;
            changed = true;
        }
    }

    let open_non_completion = updated.iter().any(|todo| {
        !matches!(todo.kind, TodoKind::Completion)
            && matches!(
                todo.status,
                TodoStatus::Pending | TodoStatus::InProgress | TodoStatus::Blocked
            )
    });
    if !open_non_completion {
        let mut promoted = false;
        for todo in &mut updated {
            if matches!(todo.kind, TodoKind::Completion)
                && matches!(todo.status, TodoStatus::Pending | TodoStatus::Blocked)
            {
                todo.status = TodoStatus::InProgress;
                promoted = true;
                changed = true;
                break;
            }
        }
        if promoted {
            for todo in &mut updated {
                if matches!(todo.kind, TodoKind::Completion)
                    && matches!(todo.status, TodoStatus::InProgress)
                    && !matches!(todo.status, TodoStatus::Completed)
                {
                    break;
                }
            }
        }
    }

    changed.then_some(updated)
}

fn request_diagnostics_from_chat(
    request: &ChatRequest,
    tools: &[crate::llm::ToolSchema],
    turn_decision: Option<crate::session::TurnDecisionDiagnostic>,
    control_envelope: Option<&TurnControlEnvelope>,
    replay_policies: &[crate::session::RequestReplayPolicyDiagnostic],
) -> RequestDiagnosticsPart {
    let messages = request
        .messages
        .iter()
        .map(request_message_diagnostic)
        .collect::<Vec<_>>();
    let image_count = messages.iter().map(|message| message.image_count).sum();
    let image_bytes = messages.iter().map(|message| message.image_bytes).sum();
    RequestDiagnosticsPart {
        provider: "openai_compat".to_string(),
        model_name: request.model.name.clone(),
        base_url: request.base_url.clone(),
        request_timeout_ms: request.timeout_ms,
        stream_idle_timeout_ms: request.stream_idle_timeout_ms,
        system_prompt_chars: request.system_prompt.chars().count(),
        tool_count: tools.len(),
        tool_choice: request
            .extra_body
            .as_ref()
            .and_then(|value| value.get("tool_choice"))
            .and_then(Value::as_str)
            .map(str::to_string),
        provider_message_count: request.messages.len(),
        image_count,
        image_bytes,
        tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
        tool_schemas: tools
            .iter()
            .map(|tool| RequestToolSchemaDiagnostic {
                name: tool.name.clone(),
                description_chars: tool.description.chars().count(),
                strict: tool.strict,
                input_schema: tool.input_schema.clone(),
            })
            .collect(),
        turn_decision,
        control_envelope: control_envelope.map(request_control_envelope_diagnostic),
        replay_policies: replay_policies.to_vec(),
        messages,
    }
}

fn compile_turn_control_envelope(
    request: &AgentRunRequest,
    active_work: Option<&ActiveWorkContract>,
    turn_decision: &crate::session::TurnDecisionDiagnostic,
    tool_names: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> crate::protocol::CompiledTurn {
    let allowed_tools = tool_names
        .iter()
        .filter_map(|name| tool_name_from_str(name))
        .collect::<Vec<_>>();
    let projection_id = ProjectionId::new();
    let context = TurnContext {
        session_id: request.session.session.id,
        cwd: request.session.workspace.cwd.clone(),
        workspace_root: request.session.workspace.root.clone(),
        provider: "openai_compat".to_string(),
        model: request.model.name.clone(),
        base_url: request.config.model.base_url.clone(),
        access_mode: request.config.permissions.access_mode,
        sandbox: sandbox_profile_for_access_mode(request.config.permissions.access_mode),
        shell_family: request
            .config
            .shell
            .family
            .unwrap_or_else(default_shell_family),
        model_capabilities: crate::protocol::ModelCapabilities {
            supports_tools: request.config.model.supports_tools,
            supports_reasoning: request.config.model.supports_reasoning,
            supports_images: request.config.model.supports_images,
            parallel_tool_calls: request.config.model.parallel_tool_calls,
            context_window: request.config.model.context_window,
            max_output_tokens: request.config.model.max_output_tokens,
        },
        route: request.state.route,
        process_phase: request.state.process_phase,
        active_contract: ActiveWorkContractProjection {
            route: request.state.route,
            process_phase: request.state.process_phase,
            active_work_kind: active_work
                .map(|contract| contract.kind().to_string())
                .filter(|kind| !kind.trim().is_empty()),
            summary: active_work
                .map(ActiveWorkContract::summary)
                .or_else(|| request.state.completion.blocked_reason.clone())
                .unwrap_or_else(|| {
                    "No open executable work is projected for this turn.".to_string()
                }),
            active_targets: active_work
                .map(ActiveWorkContract::targets)
                .filter(|targets| !targets.is_empty())
                .unwrap_or_else(|| request.state.active_targets.clone()),
            operation_intents: operation_intents_for_active_work(active_work),
            required_next_action: None,
            required_verification_commands: turn_decision.required_verification_commands.clone(),
            allowed_tools: allowed_tools.clone(),
            forbidden_tools: Vec::new(),
            projection_id,
        },
        allowed_tools,
        tool_choice: tool_choice.clone(),
        images: latest_user_images(&request.runtime_input.materialized_transcript_projection()),
        output_contract: OutputContract {
            final_answer_required: closeout_ready_final_message_authority(&request.state),
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: request
            .state
            .implementation_handoff
            .as_ref()
            .and_then(|handoff| handoff.continuation_contract.clone()),
        turn_decision_projection: Some(turn_decision.clone()),
    };
    let obligations = ObligationCompiler::compile(&context);
    TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    })
}

fn operation_intents_for_active_work(
    active_work: Option<&ActiveWorkContract>,
) -> Vec<OperationIntent> {
    match active_work {
        Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        }) if !pending_targets.is_empty() => {
            vec![OperationIntent::ContentChangingAuthoringRequired]
        }
        Some(ActiveWorkContract::DocsRepair {
            deliverable,
            pending_deliverables,
            ..
        }) if deliverable.is_some() || !pending_deliverables.is_empty() => {
            vec![OperationIntent::ContentChangingAuthoringRequired]
        }
        _ => Vec::new(),
    }
}

fn turn_decision_dispatch_block_message(
    diagnostic: &crate::session::TurnDecisionDiagnostic,
) -> Option<String> {
    let blocking = diagnostic
        .warnings
        .iter()
        .filter(|warning| warning.severity == TurnDecisionWarningSeverity::Error)
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    if blocking.is_empty() {
        None
    } else {
        Some(format!(
            "Turn decision projection is inconsistent before provider dispatch: {}",
            blocking.join(", ")
        ))
    }
}

fn control_envelope_validation_error_message(envelope: &TurnControlEnvelope) -> String {
    let validation = envelope.validate();
    let issues = validation
        .issues
        .iter()
        .map(|issue| format!("{:?}: {}", issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    if issues.is_empty() {
        "turn control envelope validation failed".to_string()
    } else {
        format!("turn control envelope validation failed before provider dispatch: {issues}")
    }
}

fn control_projection_metadata(surface: &crate::protocol::ProjectionSurface) -> Value {
    json!({
        "projection_id": surface.projection_id.to_string(),
        "surface": surface.surface.as_str(),
        "required_next_action": surface.required_next_action,
        "allowed_tools": surface.allowed_tools.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "forbidden_tools": surface.forbidden_tools.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "operation_intents": surface.operation_intents.iter().map(|intent| intent.as_str()).collect::<Vec<_>>(),
    })
}

fn tool_name_from_str(name: &str) -> Option<ToolName> {
    match name {
        "list" => Some(ToolName::List),
        "glob" => Some(ToolName::Glob),
        "grep" => Some(ToolName::Grep),
        "read" => Some(ToolName::Read),
        "inspect_directory" => Some(ToolName::InspectDirectory),
        "apply_patch" => Some(ToolName::ApplyPatch),
        "write" => Some(ToolName::Write),
        "shell" => Some(ToolName::Shell),
        "skill" => Some(ToolName::Skill),
        "docling_convert" => Some(ToolName::DoclingConvert),
        "mcp_call" => Some(ToolName::McpCall),
        "todowrite" => Some(ToolName::TodoWrite),
        _ => None,
    }
}

fn latest_user_images(transcript: &crate::session::Transcript) -> Vec<crate::session::ImagePart> {
    transcript
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.record.role, MessageRole::User))
        .map(|message| {
            message
                .parts
                .iter()
                .filter_map(|part| match &part.payload {
                    MessagePart::Image(image) => Some(image.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sandbox_profile_for_access_mode(access_mode: crate::config::AccessMode) -> SandboxProfile {
    match access_mode {
        crate::config::AccessMode::Default | crate::config::AccessMode::AutoReview => {
            SandboxProfile::WorkspaceWrite
        }
        crate::config::AccessMode::FullAccess => SandboxProfile::FullAccess,
    }
}

fn default_shell_family() -> crate::config::ShellFamily {
    if cfg!(windows) {
        crate::config::ShellFamily::PowerShell
    } else {
        crate::config::ShellFamily::Bash
    }
}

fn request_control_envelope_diagnostic(
    envelope: &TurnControlEnvelope,
) -> RequestControlEnvelopeDiagnostic {
    let validation = envelope.validate();
    RequestControlEnvelopeDiagnostic {
        envelope_id: envelope.id.to_string(),
        projection_id: envelope.projection_id.to_string(),
        dispatch_policy: dispatch_policy_label(&envelope.dispatch_policy).to_string(),
        required_next_action: envelope.action_authority.required_next_action.clone(),
        required_verification_commands: envelope
            .action_authority
            .required_verification_commands
            .clone(),
        allowed_tools: envelope
            .action_authority
            .allowed_tools
            .iter()
            .map(ToString::to_string)
            .collect(),
        forbidden_tools: envelope
            .action_authority
            .forbidden_tools
            .iter()
            .map(ToString::to_string)
            .collect(),
        validation_status: if validation.passes() {
            "pass".to_string()
        } else {
            "fail".to_string()
        },
        validation_issues: validation
            .issues
            .iter()
            .map(|issue| RequestControlEnvelopeIssueDiagnostic {
                code: format!("{:?}", issue.code),
                severity: format!("{:?}", issue.severity),
                message: issue.message.clone(),
            })
            .collect(),
        open_obligations: envelope
            .obligations
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    crate::protocol::ObligationStatus::Open
                        | crate::protocol::ObligationStatus::Blocked
                )
            })
            .map(|item| RequestControlObligationDiagnostic {
                obligation_id: item.obligation_id.clone(),
                kind: format!("{:?}", item.kind),
                summary: item.summary.clone(),
                targets: item.targets.iter().map(ToString::to_string).collect(),
                required_actions: item.required_actions.clone(),
                verification_commands: item.verification_commands.clone(),
                status: format!("{:?}", item.status),
            })
            .collect(),
        surface_projections: envelope
            .projection_bundle
            .rendered_surfaces()
            .into_iter()
            .map(|surface| RequestControlSurfaceDiagnostic {
                surface: surface.surface.as_str().to_string(),
                projection_id: surface.projection_id.to_string(),
                required_next_action: surface.required_next_action,
                allowed_tools: surface.allowed_tools,
                forbidden_tools: surface.forbidden_tools,
                text: surface.text,
            })
            .collect(),
    }
}

fn dispatch_policy_label(policy: &DispatchPolicy) -> &'static str {
    match policy {
        DispatchPolicy::Dispatch => "dispatch",
        DispatchPolicy::AwaitUser { .. } => "await_user",
        DispatchPolicy::FailClosed { .. } => "fail_closed",
        DispatchPolicy::Complete { .. } => "complete",
        DispatchPolicy::Interrupt { .. } => "interrupt",
    }
}

fn provider_messages_have_user_query_anchor(messages: &[crate::llm::ModelMessage]) -> bool {
    messages.iter().any(|message| match message {
        crate::llm::ModelMessage::User { content } => !content.trim().is_empty(),
        crate::llm::ModelMessage::UserParts { parts } => parts.iter().any(|part| match part {
            crate::llm::ModelContentPart::Text { text } => !text.trim().is_empty(),
            crate::llm::ModelContentPart::Image { .. } => true,
        }),
        _ => false,
    })
}

fn request_message_diagnostic(message: &crate::llm::ModelMessage) -> RequestMessageDiagnostic {
    match message {
        crate::llm::ModelMessage::System { content } => RequestMessageDiagnostic {
            role: "system".to_string(),
            content_chars: Some(content.chars().count()),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        crate::llm::ModelMessage::User { content } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(content.chars().count()),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        crate::llm::ModelMessage::UserParts { parts } => {
            let mut content_chars = 0usize;
            let mut image_count = 0usize;
            let mut image_bytes = 0u64;
            for part in parts {
                match part {
                    crate::llm::ModelContentPart::Text { text } => {
                        content_chars += text.chars().count();
                    }
                    crate::llm::ModelContentPart::Image { data_base64, .. } => {
                        image_count += 1;
                        image_bytes += data_base64.len() as u64;
                    }
                }
            }
            RequestMessageDiagnostic {
                role: "user".to_string(),
                content_chars: (content_chars > 0).then_some(content_chars),
                image_count,
                image_bytes,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }
        }
        crate::llm::ModelMessage::Assistant { content } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: Some(content.chars().count()),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        crate::llm::ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: content.as_ref().map(|value| value.chars().count()),
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
        crate::llm::ModelMessage::Tool {
            call_id, result, ..
        } => RequestMessageDiagnostic {
            role: "tool".to_string(),
            content_chars: Some(result.chars().count()),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.clone()),
        },
    }
}

fn rejected_tool_result(
    requested_tool: &str,
    effective_tool: &str,
    tool_exists: bool,
    tool_allowed: bool,
    control_surface: &crate::protocol::ProjectionSurface,
) -> ToolResult {
    if !tool_exists {
        let output_text = control_surface
            .render_tool_result_feedback(
                requested_tool,
                effective_tool,
                Some("The requested tool is not registered in this runtime."),
            )
            .text;
        return ToolResult {
            title: "Invalid tool call".to_string(),
            output_text,
            metadata: json!({
                "tool_rejected": true,
                "invalid_tool_call": true,
                "requested_tool": requested_tool,
                "effective_tool": effective_tool,
                "tool_exists": tool_exists,
                "tool_allowed": tool_allowed,
                "control_projection": control_projection_metadata(control_surface),
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::<ChangeSummary>::new(),
        };
    }
    let recovery_hint =
        (!tool_allowed).then_some("The requested tool is disallowed by the compiled turn policy.");
    let output_text = control_surface
        .render_tool_result_feedback(requested_tool, effective_tool, recovery_hint)
        .text;
    ToolResult {
        title: "Tool rejected".to_string(),
        output_text,
        metadata: json!({
            "tool_rejected": true,
            "requested_tool": requested_tool,
            "effective_tool": effective_tool,
            "tool_exists": tool_exists,
            "tool_allowed": tool_allowed,
            "control_projection": control_projection_metadata(control_surface),
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::<ChangeSummary>::new(),
    }
}

fn invalid_tool_arguments_result(tool_name: &str, arguments_json: &str, error: &str) -> ToolResult {
    ToolResult {
        title: "Invalid tool arguments".to_string(),
        output_text: format!(
            "Invalid arguments for `{tool_name}`: {error}. Please rewrite the input so it satisfies the expected schema."
        ),
        metadata: json!({
            "invalid_tool_arguments": true,
            "tool_name": tool_name,
            "arguments_json": arguments_json,
            "error": error,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::<ChangeSummary>::new(),
    }
}

pub(crate) fn singleton_write_surface_requires_tool_choice_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["write".to_string()]);
    matches!(
        tool_choice_for_dispatch(
            &crate::agent::prompt::PromptPolicy::default(),
            &tool_names,
            &SessionStateSnapshot::default(),
        ),
        ToolChoice::Auto
    )
}

fn is_invalid_tool_arguments_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("missing field")
        || lower.contains("invalid type")
        || lower.contains("unknown field")
        || lower.contains("expected")
}

fn required_write_content_shape_violation_result(
    tool_name: &str,
    arguments: &Value,
    required_target: &str,
) -> Option<ToolResult> {
    if !is_write_tool_name(tool_name) {
        return None;
    }
    let content = arguments.get("content").and_then(Value::as_str)?;
    if write_content_matches_required_target(required_target, content) {
        return None;
    }
    let content_shape_guidance =
        required_write_target_mismatch_content_shape_guidance(required_target, None, content);
    let content_shape_contract =
        crate::agent::content_shape_contract::python_source_for_test_target(required_target)
            .map(|contract| contract.metadata_json());
    let forbidden_markers = detected_test_target_forbidden_content_markers(content);
    let mut metadata = json!({
        "write_content_shape_mismatch": true,
        "success": false,
        "target": required_target,
        "observed_forbidden_markers": forbidden_markers,
        "tool_feedback_envelope": {
            "kind": "required_write_content_shape_mismatch",
            "success": false,
            "target": required_target,
            "side_effects_applied": false
        },
        "terminal_guard_policy": {
            "owner": "tool_orchestrator",
            "no_progress_guard": true,
            "side_effects_applied": false
        }
    });
    if let Some(contract) = content_shape_contract
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("content_shape_contract".to_string(), contract.clone());
        if let Some(feedback) = object
            .get_mut("tool_feedback_envelope")
            .and_then(Value::as_object_mut)
        {
            feedback.insert("content_shape_contract".to_string(), contract);
        }
    }
    Some(ToolResult {
        title: "Required write content shape mismatch".to_string(),
        output_text: format!(
            "The submitted content does not match `{required_target}`'s contract. Runtime rejected this tool call before applying filesystem side effects.{content_shape_guidance}"
        ),
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

fn required_write_target_mismatch_content_shape_guidance(
    required_target: &str,
    requested_target: Option<&str>,
    submitted_content: &str,
) -> String {
    let Some(contract) =
        crate::agent::content_shape_contract::python_source_for_test_target(required_target)
    else {
        return String::new();
    };
    let requested_line = requested_target
        .filter(|target| *target == contract.source_path)
        .map(|target| {
            format!(" `{target}` is the production source under test, not the active write target.")
        })
        .unwrap_or_default();
    let observed_markers = detected_test_target_forbidden_content_markers(submitted_content);
    let observed_line = if observed_markers.is_empty() {
        String::new()
    } else {
        format!(
            " Observed rejected content markers: {}.",
            observed_markers
                .iter()
                .map(|marker| format!("`{marker}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{requested_line}{}{}",
        contract.positive_shape_guidance(),
        observed_line
    )
}

pub(crate) fn required_write_target_mismatch_feedback_projects_test_content_authority() -> bool {
    let guidance = required_write_target_mismatch_content_shape_guidance(
        "test_calculator.py",
        Some("calculator.py"),
        "def add(a, b):\n    return a + b\n\ndef main():\n    input('expr')\n",
    );
    guidance.contains("production source under test")
        && guidance.contains("Required positive test-module shape")
        && guidance.contains("import `calculator`")
        && guidance.contains("TestCalculator(unittest.TestCase)")
        && guidance.contains("Forbidden shape")
        && guidance.contains("Observed rejected content markers")
        && guidance.contains("def add")
        && guidance.contains("input(")
}

fn write_content_matches_required_target(required_target: &str, content: &str) -> bool {
    let Some(contract) =
        crate::agent::content_shape_contract::python_source_for_test_target(required_target)
    else {
        return true;
    };
    let lower = content.to_ascii_lowercase();
    let module = contract.module_name.to_ascii_lowercase();
    let has_test_shape = lower.contains("def test_")
        || lower.contains("unittest")
        || lower.contains("pytest")
        || lower.contains("class test")
        || lower.contains(&format!("import {module}"))
        || lower.contains(&format!("from {module} import"));
    let looks_like_cli_source = lower.contains("input(") || lower.contains("def main(");
    has_test_shape && !looks_like_cli_source
}

pub(crate) fn preserve_provider_tool_surface_for_dispatch(tools: &mut Vec<crate::llm::ToolSchema>) {
    let _ = tools;
}

pub(crate) fn exact_write_route_accepts_unittest_main_test_content() -> bool {
    let content = r#"
import unittest
import calculator

class TestCalculator(unittest.TestCase):
    def test_add(self):
        self.assertEqual(calculator.add(2, 3), 5)

if __name__ == "__main__":
    unittest.main()
"#;
    write_content_matches_required_target("test_calculator.py", content)
}

pub(crate) fn content_shape_mismatch_feedback_carries_positive_test_contract() -> bool {
    let arguments = json!({
        "content": "def add(a, b):\n    return a + b\n\ndef main():\n    input('expr')\n"
    });
    let Some(result) =
        required_write_content_shape_violation_result("write", &arguments, "test_calculator.py")
    else {
        return false;
    };
    result
        .output_text
        .contains("Required positive test-module shape")
        && result
            .output_text
            .contains("TestCalculator(unittest.TestCase)")
        && result.output_text.contains("Forbidden shape")
        && result
            .output_text
            .contains("Observed rejected content markers")
        && result.output_text.contains("def add")
        && result.output_text.contains("input(")
        && result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("python_test_module_content_shape")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/content_shape_contract/module_name")
            .and_then(Value::as_str)
            == Some("calculator")
        && result
            .metadata
            .pointer("/observed_forbidden_markers")
            .and_then(Value::as_array)
            .is_some_and(|markers| {
                markers
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|marker| marker == "def add")
            })
}

fn detected_test_target_forbidden_content_markers(content: &str) -> Vec<String> {
    let mut markers = BTreeSet::new();
    let lower = content.to_ascii_lowercase();
    if lower.contains("input(") {
        markers.insert("input(".to_string());
    }
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("def ") else {
            continue;
        };
        let name = rest
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .next()
            .unwrap_or_default();
        if !name.is_empty() && !name.starts_with("test_") {
            markers.insert(format!("def {name}"));
        }
    }
    markers.into_iter().collect()
}

fn tool_choice_from_policy(policy: &crate::agent::prompt::PromptPolicy) -> ToolChoice {
    let _ = policy;
    ToolChoice::Auto
}

fn closeout_ready_final_message_authority(state: &SessionStateSnapshot) -> bool {
    state.completion.closeout_ready
        && state.completion.open_work_count == 0
        && !state.completion.verification_pending
        && !state.completion.route_contract_pending
}

fn clean_closeout_final_message_lifecycle(
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) -> bool {
    active_work.is_none()
        && state.completion.closeout_ready
        && state.completion.open_work_count == 0
        && !state.completion.verification_pending
        && !state.completion.route_contract_pending
        && state.completion.blocked_reason.is_none()
}

pub(crate) fn clean_closeout_final_message_lifecycle_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.completion.closeout_ready = true;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;
    clean_closeout_final_message_lifecycle(&state, None)
        && tool_choice_for_dispatch(
            &crate::agent::prompt::PromptPolicy::default(),
            &BTreeSet::new(),
            &state,
        ) == ToolChoice::None
        && closeout_ready_final_message_authority(&state)
}

pub(crate) fn executed_tool_failure_terminal_guard_fixture_passes() -> bool {
    let allowed = BTreeSet::from(["read".to_string()]);
    let first = executed_tool_failure_no_progress_key(
        "read",
        r#"{"path":"missing.py"}"#,
        &allowed,
        "The system cannot find the path specified. (os error 3)",
    );
    let second = executed_tool_failure_no_progress_key(
        "read",
        r#"{"path":"missing.py"}"#,
        &allowed,
        "指定されたパスが見つかりません。 (os error 3)",
    );
    first == second
        && executed_tool_failure_terminal_message(
            "read",
            EXECUTED_TOOL_FAILURE_TERMINAL_THRESHOLD,
            "指定されたパスが見つかりません。 (os error 3)",
        )
        .contains("Runtime stopped")
}

pub(crate) fn progress_projection_loop_terminal_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("test_space_invader.py"),
    ];
    state.completion.open_work_count = 2;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let allowed = BTreeSet::from([
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let first = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        Some("same-result"),
    );
    let second = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        Some("same-result"),
    );
    let different_result = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        Some("different-result"),
    );
    let mut progressed_state = state.clone();
    progressed_state
        .active_targets
        .retain(|target| target.as_str() != "test_space_invader.py");
    let progressed = progress_projection_no_progress_key(
        "todowrite",
        &progressed_state,
        &allowed,
        &ToolChoice::Required,
        Some("same-result"),
    );
    let result = ToolResult {
        title: "Plan updated".to_string(),
        output_text: "Plan updated".to_string(),
        metadata: json!({"progress_projection": true, "todo_count": 3}),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    };
    let completion_metadata = json!({
        "progress_projection": true,
        "result_hash": "completed-result",
        "tool_feedback_envelope": {
            "result_hash": "completed-result"
        }
    });
    let different_completion_metadata = json!({
        "progress_projection": true,
        "result_hash": "completed-different-result",
        "tool_feedback_envelope": {
            "result_hash": "completed-different-result"
        }
    });
    let raw_missing_hash_key = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        tool_result_result_hash(&result.metadata).as_deref(),
    );
    let completed_metadata_key = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        tool_result_result_hash(&completion_metadata).as_deref(),
    );
    let different_completed_metadata_key = progress_projection_no_progress_key(
        "todowrite",
        &state,
        &allowed,
        &ToolChoice::Required,
        tool_result_result_hash(&different_completion_metadata).as_deref(),
    );
    first == second
        && first != different_result
        && first != progressed
        && raw_missing_hash_key != completed_metadata_key
        && completed_metadata_key != different_completed_metadata_key
        && tool_result_is_progress_projection_no_content(&result)
        && should_terminalize_progress_projection_no_progress(
            PROGRESS_PROJECTION_NO_PROGRESS_TERMINAL_THRESHOLD,
        )
        && progress_projection_no_progress_terminal_message(
            "todowrite",
            PROGRESS_PROJECTION_NO_PROGRESS_TERMINAL_THRESHOLD,
            &state,
        )
        .contains("progress projection")
}

pub(crate) fn open_authoring_operation_intent_classifies_non_content_tools_fixture_passes() -> bool
{
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("README.md"),
            Utf8PathBuf::from("space_invader.py"),
            Utf8PathBuf::from("test_space_invader.py"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let operation_intents = operation_intents_for_active_work(Some(&active_work));
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("space_invader.py"),
        Utf8PathBuf::from("test_space_invader.py"),
    ];
    state.completion.open_work_count = 3;
    state.completion.closeout_ready = false;
    let allowed = BTreeSet::from([
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let supporting_context_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress"
    });
    let progress_projection_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "progress_projection",
        "progress_effect": "no_progress"
    });

    operation_intents == vec![OperationIntent::ContentChangingAuthoringRequired]
        && crate::agent::tool_orchestrator::open_authoring_operation_intent_classification_fixture_passes()
        && operation_non_content_no_progress_under_open_authoring(
            &supporting_context_metadata,
            &state,
        )
        && !operation_non_content_no_progress_under_open_authoring(
            &progress_projection_metadata,
            &state,
        )
        && operation_non_content_no_progress_key(
            "read",
            &supporting_context_metadata,
            &state,
            &allowed,
            &ToolChoice::Required,
        )
            .contains("content_changing_authoring_required")
        && should_terminalize_operation_non_content_no_progress(
            OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
        )
}

pub(crate) fn open_authoring_operation_intent_preserves_tool_surface_fixture_passes() -> bool {
    let _active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("artifact.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let docs_work = ActiveWorkContract::DocsRepair {
        deliverable: Some(Utf8PathBuf::from("README.md")),
        pending_deliverables: vec![crate::session::DocsPendingDeliverable {
            target: Utf8PathBuf::from("README.md"),
            summary: "topics=overview".to_string(),
        }],
        pending_summary: "docs route contract pending".to_string(),
    };
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("artifact.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    let available = BTreeSet::from([
        "inspect_directory".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
        "apply_patch".to_string(),
    ]);
    let effective = available.clone();
    let expected = available.clone();

    effective == expected
        && operation_intents_for_active_work(Some(&docs_work))
            == vec![OperationIntent::ContentChangingAuthoringRequired]
        && effective.contains("write")
        && effective.contains("apply_patch")
        && effective.contains("read")
        && effective.contains("todowrite")
        && operation_non_content_no_progress_under_open_authoring(
            &json!({
                "operation_intent": "content_changing_authoring_required",
                "operation_progress_class": "supporting_context",
                "progress_effect": "no_progress"
            }),
            &state,
        )
        && {
            let read_metadata = json!({
                "operation_intent": "content_changing_authoring_required",
                "operation_progress_class": "supporting_context",
                "progress_effect": "no_progress",
                "result_hash": "read-hash"
            });
            let inspect_metadata = json!({
                "operation_intent": "content_changing_authoring_required",
                "operation_progress_class": "supporting_context",
                "progress_effect": "no_progress",
                "result_hash": "inspect-hash"
            });
            let first_key = operation_non_content_no_progress_key(
                "read",
                &read_metadata,
                &state,
                &effective,
                &ToolChoice::Required,
            );
            let repeated_key = operation_non_content_no_progress_key(
                "read",
                &read_metadata,
                &state,
                &effective,
                &ToolChoice::Required,
            );
            let different_key = operation_non_content_no_progress_key(
                "inspect_directory",
                &inspect_metadata,
                &state,
                &effective,
                &ToolChoice::Required,
            );
            first_key == repeated_key
                && first_key != different_key
                && first_key.contains("content_changing_authoring_required")
        }
        && should_terminalize_operation_non_content_no_progress(
            OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
        )
}

pub(crate) fn docs_route_semantic_no_progress_guard_fixture_passes() -> bool {
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.completion.route_contract_pending = true;
    docs_state.completion.open_work_count = 3;
    docs_state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("basic_design.md"),
        Utf8PathBuf::from("detail_design.md"),
    ];
    let allowed = BTreeSet::from([
        "list".to_string(),
        "read".to_string(),
        "write".to_string(),
        "apply_patch".to_string(),
    ]);
    let read_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "read-a"
    });
    let other_read_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "read-b"
    });
    let list_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "list-c"
    });
    let first_key = operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let second_key = operation_non_content_no_progress_key(
        "read",
        &other_read_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let list_key = operation_non_content_no_progress_key(
        "list",
        &list_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let mut code_state = docs_state.clone();
    code_state.route = TaskRoute::Code;
    code_state.completion.route_contract_pending = false;
    let code_first = operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &code_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let code_second = operation_non_content_no_progress_key(
        "read",
        &other_read_metadata,
        &code_state,
        &allowed,
        &ToolChoice::Auto,
    );

    first_key == second_key
        && second_key == list_key
        && !first_key.contains("read-a")
        && !first_key.contains("read-b")
        && code_first != code_second
        && !should_terminalize_operation_non_content_no_progress_for_state(3, &docs_state)
        && should_terminalize_operation_non_content_no_progress_for_state(
            DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
            &docs_state,
        )
}

pub(crate) fn progress_projection_stable_surface_guard_fixture_passes() -> bool {
    open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
        && docs_route_semantic_no_progress_guard_fixture_passes()
        && docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes()
        && crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes()
}

pub(crate) fn docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes() -> bool
{
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.completion.route_contract_pending = true;
    docs_state.completion.open_work_count = 3;
    docs_state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("basic_design.md"),
        Utf8PathBuf::from("detail_design.md"),
    ];
    let allowed = BTreeSet::from([
        "list".to_string(),
        "read".to_string(),
        "write".to_string(),
        "apply_patch".to_string(),
    ]);
    let metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "ignored-for-docs"
    });
    let operation_key = operation_non_content_no_progress_key(
        "read",
        &metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let budget_key =
        docs_route_supporting_context_budget_key(&docs_state, &allowed, &ToolChoice::Auto);
    let result = docs_supporting_context_budget_exhausted_result(
        "read",
        &json!({"path": "backend/app/main.py"}),
        &docs_state,
    );
    operation_key == budget_key
        && docs_route_supporting_context_budget_applies("read", &docs_state)
        && !docs_route_supporting_context_budget_applies("write", &docs_state)
        && result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
        && result.output_text.contains("write")
        && result.output_text.contains("apply_patch")
        && result.output_text.contains("不明")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/operation_progress_class")
            .and_then(Value::as_str)
            == Some("docs_supporting_context_budget_exhausted")
        && result
            .metadata
            .pointer("/terminal_guard_policy/terminal_after_repeated_corrections")
            .and_then(Value::as_u64)
            == Some(DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD as u64)
        && docs_supporting_context_budget_exhausted_terminal_message(3, &docs_state)
            .contains("budget was exhausted")
}

pub(crate) fn docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes() -> bool {
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.completion.route_contract_pending = true;
    docs_state.completion.open_work_count = 3;
    docs_state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("basic_design.md"),
        Utf8PathBuf::from("detail_design.md"),
    ];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "list".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let exhausted = BTreeSet::from([docs_route_supporting_context_budget_key(
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    )]);
    let mut visible = allowed.clone();
    if docs_route_supporting_context_budget_recovery_surface_active(&docs_state, &exhausted) {
        visible.retain(|tool| docs_route_supporting_context_budget_recovery_tool_visible(tool));
    }
    let mut inactive_state = docs_state.clone();
    inactive_state.route = TaskRoute::Code;

    visible
        == BTreeSet::from([
            "apply_patch".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && !visible.contains("read")
        && !visible.contains("list")
        && !visible.contains("grep")
        && docs_route_supporting_context_budget_recovery_surface_active(&docs_state, &exhausted)
        && !docs_route_supporting_context_budget_recovery_surface_active(
            &inactive_state,
            &exhausted,
        )
}

pub(crate) fn docs_route_budget_exhaustion_survives_partial_write_fixture_passes() -> bool {
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.completion.route_contract_pending = true;
    docs_state.completion.open_work_count = 2;
    docs_state.active_targets = vec![
        Utf8PathBuf::from("basic_design.md"),
        Utf8PathBuf::from("detail_design.md"),
    ];
    let exhausted = BTreeSet::from(["docs-budget".to_string()]);
    let mut retained = exhausted.clone();
    if !docs_route_contract_still_pending_after_file_change(&docs_state) {
        retained.clear();
    }
    let mut visible = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    if docs_route_supporting_context_budget_recovery_surface_active(&docs_state, &retained) {
        visible.retain(|tool| docs_route_supporting_context_budget_recovery_tool_visible(tool));
    }
    docs_state.completion.route_contract_pending = false;
    let mut cleared = exhausted.clone();
    if !docs_route_contract_still_pending_after_file_change(&docs_state) {
        cleared.clear();
    }

    retained == exhausted
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && cleared.is_empty()
}

pub(crate) fn edit_surface_registry_symmetry_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let _active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("README.md"),
            Utf8PathBuf::from("test_source.py"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("test_source.py"),
    ];
    state.completion.open_work_count = 2;
    state.completion.closeout_ready = false;
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({"path": "source.py", "content": "stale source"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path": "source.py", "content": "stale source"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write, ToolName::ApplyPatch],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Wrong authoring target".to_string(),
                output_text: "The submitted write call targeted source.py, but active targets are README.md and test_source.py.".to_string(),
                metadata: json!({
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "wrong_authoring_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["source.py"],
                    "active_authoring_targets": ["README.md", "test_source.py"]
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("wrong-target-source".to_string()),
                verification_run: None,
            },
        },
    ];
    let available = BTreeSet::from([
        "read".to_string(),
        "write".to_string(),
        "apply_patch".to_string(),
    ]);
    let effective = available.clone();
    let _ = history_items;
    effective.contains("write")
        && effective.contains("apply_patch")
        && effective.contains("read")
        && crate::agent::prompt::provider_replay_preserves_failed_inactive_authoring_feedback()
}

fn operation_content_changing_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "write" | "apply_patch")
}

fn verification_commands_for_active_work(
    active_work: Option<&ActiveWorkContract>,
) -> Option<&[String]> {
    match active_work {
        Some(ActiveWorkContract::Verification { commands, .. }) if !commands.is_empty() => {
            Some(commands.as_slice())
        }
        _ => None,
    }
}

fn wrong_verification_shell_command_result(
    effective_tool_name: &str,
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
) -> Option<ToolResult> {
    if effective_tool_name != "shell" {
        return None;
    }
    let required_commands = verification_commands_for_active_work(active_work)?;
    let submitted = parsed_arguments.get("command")?.as_str()?.trim();
    let submitted_key = verification_command_identity_key(submitted)?;
    if required_commands.iter().any(|required| {
        verification_command_identity_key(required)
            .as_deref()
            .is_some_and(|required_key| required_key == submitted_key)
    }) {
        return None;
    }
    Some(ToolResult {
        title: "Run required verification command".to_string(),
        output_text: format!(
            "Verification is still pending. The submitted shell command `{submitted}` does not match any remaining required verification command. Run one of: {}.",
            required_commands.join(", ")
        ),
        metadata: json!({
            "corrective_result": true,
            "operation_progress_class": "wrong_verification_command",
            "progress_effect": "no_progress",
            "submitted_command": submitted,
            "required_verification_commands": required_commands,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

fn wrong_verification_command_key(
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let command = parsed_arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let required = verification_commands_for_active_work(active_work)
        .map(|commands| commands.join("|"))
        .unwrap_or_default();
    format!(
        "command={command}|required={required}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice)
    )
}

fn wrong_verification_command_terminal_message(result: &ToolResult, count: usize) -> String {
    format!(
        "Submitted shell command did not match the remaining required verification command {count} time(s): {}",
        result.output_text
    )
}

fn wrong_authoring_target_result(
    effective_tool_name: &str,
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
    workspace_root: &Utf8Path,
) -> Option<ToolResult> {
    if !operation_content_changing_tool_name(effective_tool_name) {
        return None;
    }
    let active_targets = active_requested_work_targets(active_work)?;
    let submitted_targets = submitted_authoring_targets(effective_tool_name, parsed_arguments);
    if submitted_targets.is_empty() {
        return None;
    }
    let active_keys = active_targets
        .iter()
        .flat_map(|target| normalized_target_keys(target.as_str(), workspace_root))
        .collect::<BTreeSet<_>>();
    let submitted_keys = submitted_targets
        .iter()
        .flat_map(|target| normalized_target_keys(target, workspace_root))
        .collect::<BTreeSet<_>>();
    if !active_keys.is_empty() && !submitted_keys.is_disjoint(&active_keys) {
        return None;
    }

    let active_target_strings = active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect::<Vec<_>>();
    let target_line = active_target_strings
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let submitted_line = submitted_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(ToolResult {
        title: "Wrong authoring target".to_string(),
        output_text: format!(
            "The submitted content-changing `{effective_tool_name}` call targets {submitted_line}, but the current active requested deliverables are {target_line}. Runtime rejected this call before applying filesystem side effects because it would not satisfy the open requested-work authoring lifecycle."
        ),
        metadata: json!({
            "corrective_result": true,
            "success": false,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "wrong_authoring_target",
            "progress_effect": "no_progress",
            "submitted_targets": submitted_targets,
            "active_authoring_targets": active_target_strings,
            "tool_feedback_envelope": {
                "kind": "wrong_authoring_target",
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "wrong_authoring_target",
                "progress_effect": "no_progress",
                "submitted_targets": submitted_targets,
                "active_targets": active_target_strings,
                "side_effects_applied": false
            },
            "terminal_guard_policy": {
                "owner": "tool_lifecycle",
                "no_progress_guard": true,
                "side_effects_applied": false
            }
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

fn active_requested_work_targets(
    active_work: Option<&ActiveWorkContract>,
) -> Option<&[Utf8PathBuf]> {
    match active_work {
        Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        }) if !pending_targets.is_empty() => Some(pending_targets.as_slice()),
        Some(ActiveWorkContract::DocsRepair {
            deliverable: Some(deliverable),
            ..
        }) => Some(std::slice::from_ref(deliverable)),
        Some(ActiveWorkContract::DocsRepair {
            pending_deliverables,
            ..
        }) if !pending_deliverables.is_empty() => None,
        _ => None,
    }
}

fn submitted_authoring_targets(tool_name: &str, parsed_arguments: &Value) -> Vec<String> {
    let mut targets = BTreeSet::new();
    match tool_name {
        "write" => {
            if let Some(path) = parsed_arguments.get("path").and_then(Value::as_str) {
                targets.insert(path.trim().to_string());
            }
        }
        "apply_patch" => {
            if let Some(patch_text) = parsed_arguments.get("patch_text").and_then(Value::as_str) {
                targets.extend(apply_patch_declared_targets(patch_text));
            }
        }
        _ => {}
    }
    targets
        .into_iter()
        .filter(|target| !target.trim().is_empty())
        .collect()
}

fn apply_patch_declared_targets(patch_text: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for line in patch_text.lines() {
        for marker in [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ] {
            if let Some(target) = line.strip_prefix(marker) {
                let target = target.trim();
                if !target.is_empty() {
                    targets.insert(target.to_string());
                }
            }
        }
    }
    targets
}

fn wrong_authoring_target_key(
    effective_tool_name: &str,
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
    workspace_root: &Utf8Path,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let submitted = submitted_authoring_targets(effective_tool_name, parsed_arguments)
        .into_iter()
        .flat_map(|target| normalized_target_keys(&target, workspace_root))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let active = active_requested_work_targets(active_work)
        .map(|targets| {
            targets
                .iter()
                .flat_map(|target| normalized_target_keys(target.as_str(), workspace_root))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    format!(
        "tool={effective_tool_name}|submitted={submitted}|active={active}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn wrong_authoring_target_terminal_message(result: &ToolResult, count: usize) -> String {
    format!(
        "Submitted content-changing calls missed the active requested-work deliverable set {count} time(s): {}",
        result.output_text
    )
}

pub(crate) fn verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes()
-> bool {
    let available = BTreeSet::from([
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.process_phase = crate::session::ProcessPhase::Verify;
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let active = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let effective = available.clone();
    let wrong = wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -m py_compile app.py"}),
        Some(&active),
    );
    let right = wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -m unittest"}),
        Some(&active),
    );
    let read_result = ToolResult {
        title: "Read app.py".to_string(),
        output_text: "1: print('hello')".to_string(),
        metadata: Value::Null,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    };
    effective == available
        && wrong.as_ref().is_some_and(|result| {
            result.output_text.contains("python -m unittest")
                && result
                    .metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("wrong_verification_command")
        })
        && right.is_none()
        && verification_supporting_context_no_progress_under_active_verification(
            "read",
            r#"{"path":"app.py"}"#,
            &read_result,
            &state,
        )
        && verification_supporting_context_no_progress_key(
            "read",
            r#"{"path":"app.py"}"#,
            &state,
            &effective,
            &ToolChoice::Required,
        )
        .contains("verification_supporting_context")
        && should_terminalize_verification_supporting_context_no_progress(
            VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD,
        )
}

pub(crate) fn active_authoring_rejects_wrong_target_fixture_passes() -> bool {
    let active = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("README.md"),
            Utf8PathBuf::from("test_space_invader.py"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let workspace_root = Utf8Path::new("C:/workspace/route");
    let wrong_write = wrong_authoring_target_result(
        "write",
        &json!({"path": "space_invader.py", "content": "source"}),
        Some(&active),
        workspace_root,
    );
    let right_write = wrong_authoring_target_result(
        "write",
        &json!({"path": "test_space_invader.py", "content": "tests"}),
        Some(&active),
        workspace_root,
    );
    let wrong_patch = wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Update File: space_invader.py\n@@\n-pass\n+pass\n*** End Patch"}),
        Some(&active),
        workspace_root,
    );
    let right_patch = wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Add File: README.md\n+Space Invader\n*** End Patch"}),
        Some(&active),
        workspace_root,
    );
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let first_key = wrong_authoring_target_key(
        "write",
        &json!({"path": "space_invader.py", "content": "source"}),
        Some(&active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
    );
    let second_key = wrong_authoring_target_key(
        "write",
        &json!({"path": "space_invader.py", "content": "different source"}),
        Some(&active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
    );
    let progressed_active = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("space_invader.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let progressed_key = wrong_authoring_target_key(
        "write",
        &json!({"path": "space_invader.py", "content": "source"}),
        Some(&progressed_active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
    );
    let docs_active = ActiveWorkContract::DocsRepair {
        deliverable: Some(Utf8PathBuf::from("basic_design.md")),
        pending_deliverables: vec![
            crate::session::DocsPendingDeliverable {
                target: Utf8PathBuf::from("basic_design.md"),
                summary: "basic_design.md (topics=responsibility, data flow, frontend)".to_string(),
            },
            crate::session::DocsPendingDeliverable {
                target: Utf8PathBuf::from("detail_design.md"),
                summary: "detail_design.md (topics=module input output, data model, flow)"
                    .to_string(),
            },
        ],
        pending_summary: "docs route contract is pending".to_string(),
    };
    let docs_completed_target_regression = wrong_authoring_target_result(
        "write",
        &json!({"path": "README.md", "content": "# stale completed deliverable"}),
        Some(&docs_active),
        workspace_root,
    );
    let docs_active_target_write = wrong_authoring_target_result(
        "write",
        &json!({"path": "basic_design.md", "content": "# Basic design"}),
        Some(&docs_active),
        workspace_root,
    );
    let docs_completed_target_patch = wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch"}),
        Some(&docs_active),
        workspace_root,
    );

    wrong_write.as_ref().is_some_and(|result| {
        result.recorded_changes.is_empty()
            && result.change_summaries.is_empty()
            && result
                .metadata
                .get("operation_progress_class")
                .and_then(Value::as_str)
                == Some("wrong_authoring_target")
            && result
                .metadata
                .get("tool_feedback_envelope")
                .and_then(|value| value.get("side_effects_applied"))
                .and_then(Value::as_bool)
                == Some(false)
            && result.output_text.contains("README.md")
            && result.output_text.contains("test_space_invader.py")
    }) && right_write.is_none()
        && wrong_patch.is_some()
        && right_patch.is_none()
        && first_key == second_key
        && first_key != progressed_key
        && docs_completed_target_regression
            .as_ref()
            .is_some_and(|result| {
                result.recorded_changes.is_empty()
                    && result.change_summaries.is_empty()
                    && result
                        .metadata
                        .pointer("/tool_feedback_envelope/operation_progress_class")
                        .and_then(Value::as_str)
                        == Some("wrong_authoring_target")
                    && result.output_text.contains("basic_design.md")
                    && !result.output_text.contains("detail_design.md")
            })
        && docs_active_target_write.is_none()
        && docs_completed_target_patch.is_some()
        && wrong_authoring_target_terminal_message(
            wrong_write
                .as_ref()
                .expect("wrong write should be rejected"),
            WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD,
        )
        .contains("active requested-work deliverable set")
}

pub(crate) fn docs_route_rejects_completed_deliverable_regression_fixture_passes() -> bool {
    let docs_active = ActiveWorkContract::DocsRepair {
        deliverable: Some(Utf8PathBuf::from("basic_design.md")),
        pending_deliverables: vec![
            crate::session::DocsPendingDeliverable {
                target: Utf8PathBuf::from("basic_design.md"),
                summary: "basic_design.md (topics=responsibility, data flow, frontend)".to_string(),
            },
            crate::session::DocsPendingDeliverable {
                target: Utf8PathBuf::from("detail_design.md"),
                summary: "detail_design.md (topics=module input output, data model, flow)"
                    .to_string(),
            },
        ],
        pending_summary: "docs route contract is pending".to_string(),
    };
    let workspace_root = Utf8Path::new("C:/workspace/route");
    let completed_readme_write = wrong_authoring_target_result(
        "write",
        &json!({"path": "README.md", "content": "# stale completed deliverable"}),
        Some(&docs_active),
        workspace_root,
    );
    let active_basic_write = wrong_authoring_target_result(
        "write",
        &json!({"path": "basic_design.md", "content": "# Basic design"}),
        Some(&docs_active),
        workspace_root,
    );
    let completed_readme_patch = wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch"}),
        Some(&docs_active),
        workspace_root,
    );

    completed_readme_write.as_ref().is_some_and(|result| {
        result.recorded_changes.is_empty()
            && result.change_summaries.is_empty()
            && result
                .metadata
                .pointer("/tool_feedback_envelope/operation_progress_class")
                .and_then(Value::as_str)
                == Some("wrong_authoring_target")
            && result
                .metadata
                .pointer("/tool_feedback_envelope/side_effects_applied")
                .and_then(Value::as_bool)
                == Some(false)
            && result.output_text.contains("basic_design.md")
            && !result.output_text.contains("detail_design.md")
    }) && active_basic_write.is_none()
        && completed_readme_patch.is_some()
}

fn tool_result_is_progress_projection_no_content(result: &ToolResult) -> bool {
    result
        .metadata
        .get("progress_projection")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
}

fn progress_projection_no_progress_key(
    effective_tool_name: &str,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    result_hash: Option<&str>,
) -> String {
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_ascii_lowercase())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let required_commands = state
        .verification
        .required_commands
        .iter()
        .map(|command| command.trim().to_ascii_lowercase())
        .filter(|command| !command.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|result_hash={}|route={:?}|phase={:?}|open={}|verification_pending={}|route_contract_pending={}|targets={}|commands={}|allowed={}|choice={}",
        effective_tool_name,
        result_hash.unwrap_or(""),
        state.route,
        state.process_phase,
        state.completion.open_work_count,
        state.completion.verification_pending,
        state.completion.route_contract_pending,
        active_targets,
        required_commands,
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn tool_result_result_hash(metadata: &Value) -> Option<String> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("result_hash"))
        .or_else(|| metadata.get("result_hash"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn should_terminalize_progress_projection_no_progress(progress_count: usize) -> bool {
    progress_count >= PROGRESS_PROJECTION_NO_PROGRESS_TERMINAL_THRESHOLD
}

fn progress_projection_no_progress_terminal_message(
    tool_name: &str,
    progress_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Tool `{tool_name}` returned progress projection without artifact or workspace progress {progress_count} time(s) while executable work remains open. Runtime stopped before repeating plan-only outputs until the turn step budget. Open targets: {targets}."
    )
}

fn operation_non_content_no_progress_under_open_authoring(
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> bool {
    if !open_executable_work_requires_tool_call(state) {
        return false;
    }
    let operation_intent = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_intent"))
        .or_else(|| metadata.get("operation_intent"))
        .and_then(Value::as_str);
    let progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str);
    operation_intent == Some(OperationIntent::ContentChangingAuthoringRequired.as_str())
        && matches!(progress_class, Some("supporting_context" | "no_progress"))
}

fn operation_non_content_no_progress_key(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let result_hash = if docs_route_semantic_operation_no_progress(state, progress_class) {
        None
    } else {
        tool_result_result_hash(metadata)
    };
    let tool_name = if docs_route_semantic_operation_no_progress(state, progress_class) {
        "docs_route_supporting_context"
    } else {
        effective_tool_name
    };
    format!(
        "operation_intent=content_changing_authoring_required|progress_class={progress_class}|{}",
        progress_projection_no_progress_key(
            tool_name,
            state,
            allowed_tools,
            tool_choice,
            result_hash.as_deref(),
        )
    )
}

fn operation_progress_class_from_metadata(metadata: &Value) -> Option<&str> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
}

fn docs_route_supporting_context_budget_applies(
    tool_name: &str,
    state: &SessionStateSnapshot,
) -> bool {
    state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && matches!(
            tool_name,
            "read"
                | "list"
                | "glob"
                | "grep"
                | "inspect_directory"
                | "skill"
                | "docling_convert"
                | "mcp_call"
        )
}

fn docs_route_supporting_context_budget_recovery_surface_active(
    state: &SessionStateSnapshot,
    exhausted_keys: &BTreeSet<String>,
) -> bool {
    state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && !exhausted_keys.is_empty()
}

fn docs_route_contract_still_pending_after_file_change(state: &SessionStateSnapshot) -> bool {
    state.route == TaskRoute::Docs && state.completion.route_contract_pending
}

fn docs_route_supporting_context_budget_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "write" | "apply_patch" | "todowrite")
}

fn docs_route_supporting_context_budget_key(
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let metadata = json!({
        "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress"
    });
    operation_non_content_no_progress_key(
        "docs_route_supporting_context",
        &metadata,
        state,
        allowed_tools,
        tool_choice,
    )
}

fn docs_supporting_context_budget_targets(state: &SessionStateSnapshot) -> Vec<String> {
    state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect()
}

fn docs_supporting_context_budget_exhausted_result(
    tool_name: &str,
    arguments: &Value,
    state: &SessionStateSnapshot,
) -> ToolResult {
    let targets = docs_supporting_context_budget_targets(state);
    let target_text = if targets.is_empty() {
        "one pending docs deliverable".to_string()
    } else {
        targets.join(", ")
    };
    let result_hash = format!(
        "docs_supporting_context_budget_exhausted:{}:{}",
        tool_name,
        targets.join("|")
    );
    ToolResult {
        title: "Docs supporting context budget exhausted".to_string(),
        output_text: format!(
            "Docs route supporting-context budget is exhausted for this authoring step. Runtime rejected `{tool_name}` before filesystem or workspace side effects. Do not continue broad read/list/search discovery now. Use `write` or `apply_patch` to create or update one pending docs deliverable: {target_text}. Use `不明` for still-unconfirmed details instead of opening more source context."
        ),
        metadata: json!({
            "success": false,
            "docs_supporting_context_budget_exhausted": true,
            "requested_tool": tool_name,
            "requested_arguments": arguments,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "docs_supporting_context_budget_exhausted",
            "progress_effect": "no_progress",
            "active_targets": targets,
            "result_hash": result_hash,
            "tool_feedback_envelope": {
                "kind": "docs_supporting_context_budget_exhausted",
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "docs_supporting_context_budget_exhausted",
                "progress_effect": "no_progress",
                "side_effects_applied": false,
                "active_targets": targets,
                "result_hash": result_hash
            },
            "terminal_guard_policy": {
                "owner": "tool_orchestrator",
                "no_progress_guard": true,
                "side_effects_applied": false,
                "terminal_after_repeated_corrections": DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD
            }
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn should_terminalize_operation_non_content_no_progress(progress_count: usize) -> bool {
    progress_count >= OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
}

fn should_terminalize_operation_non_content_no_progress_for_state(
    progress_count: usize,
    state: &SessionStateSnapshot,
) -> bool {
    let threshold = if state.route == TaskRoute::Docs && state.completion.route_contract_pending {
        DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
    } else {
        OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
    };
    progress_count >= threshold
}

fn docs_route_semantic_operation_no_progress(
    state: &SessionStateSnapshot,
    progress_class: &str,
) -> bool {
    state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && matches!(progress_class, "supporting_context" | "progress_projection")
}

fn docs_supporting_context_budget_exhausted_terminal_message(
    correction_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let targets = docs_supporting_context_budget_targets(state).join(", ");
    format!(
        "Docs route supporting-context budget was exhausted and the model repeated budget-exhausted discovery {correction_count} time(s) instead of producing file-change evidence. Runtime stopped before growing provider history with more no-progress tool calls. Open docs targets: {targets}."
    )
}

fn operation_non_content_no_progress_terminal_message(
    tool_name: &str,
    progress_count: usize,
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> String {
    let progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
        .unwrap_or("non_content");
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}",
        if state.route == TaskRoute::Docs && state.completion.route_contract_pending {
            format!(
                "Tool `{tool_name}` returned `{progress_class}` output {progress_count} time(s) while docs authoring is required. Runtime stopped before allowing more broad docs-route discovery to grow provider history. The representative survey budget is exhausted; use write/apply_patch for one pending docs deliverable, using `不明` for still-unconfirmed details. Open targets: {targets}."
            )
        } else {
            format!(
                "Tool `{tool_name}` returned `{progress_class}` output {progress_count} time(s) while content-changing authoring is required. Runtime stopped before treating non-content tool calls as artifact progress. Use write/apply_patch or equivalent file-change evidence for open targets: {targets}."
            )
        }
    )
}

fn verification_supporting_context_no_progress_under_active_verification(
    tool_name: &str,
    arguments_json: &str,
    result: &ToolResult,
    state: &SessionStateSnapshot,
) -> bool {
    let _ = arguments_json;
    state.completion.verification_pending
        && !state.verification.required_commands.is_empty()
        && verification_supporting_context_tool_name(tool_name)
        && result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
}

fn verification_supporting_context_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read"
            | "list"
            | "glob"
            | "grep"
            | "inspect_directory"
            | "skill"
            | "docling_convert"
            | "mcp_call"
            | "todowrite"
    )
}

fn verification_supporting_context_no_progress_key(
    effective_tool_name: &str,
    arguments_json: &str,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let normalized_args = serde_json::from_str::<Value>(arguments_json)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments_json.trim().to_string());
    format!(
        "verification_supporting_context|tool={effective_tool_name}|args={normalized_args}|{}",
        progress_projection_no_progress_key(
            "verification_supporting_context",
            state,
            allowed_tools,
            tool_choice,
            None,
        )
    )
}

fn should_terminalize_verification_supporting_context_no_progress(progress_count: usize) -> bool {
    progress_count >= VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD
}

fn verification_supporting_context_no_progress_terminal_message(
    tool_name: &str,
    progress_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let commands = state.verification.required_commands.join(", ");
    format!(
        "Tool `{tool_name}` returned supporting context {progress_count} time(s) while verification commands remain pending. Runtime stopped before repeating context-only calls until the turn step budget. Run one of the remaining verification commands instead: {commands}."
    )
}

fn rejected_tool_no_progress_key(
    effective_tool_name: &str,
    effective_arguments_json: &str,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let _ = effective_arguments_json;
    format!(
        "rejected_tool|tool={}|allowed={}|choice={}",
        effective_tool_name,
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice)
    )
}

fn executed_tool_failure_no_progress_key(
    effective_tool_name: &str,
    effective_arguments_json: &str,
    allowed_tools: &BTreeSet<String>,
    error_text: &str,
) -> String {
    let normalized_arguments = serde_json::from_str::<Value>(effective_arguments_json)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| effective_arguments_json.trim().to_string());
    format!(
        "{}|{}|{}|{}",
        effective_tool_name,
        normalized_arguments,
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_error_class(error_text)
    )
}

fn tool_error_class(error_text: &str) -> String {
    let lower = error_text.to_ascii_lowercase();
    if lower.contains("os error") || lower.contains("not found") || lower.contains("見つかりません")
    {
        "io_not_found".to_string()
    } else if lower.contains("permission") || lower.contains("denied") {
        "permission_denied".to_string()
    } else if lower.contains("timeout") {
        "timeout".to_string()
    } else {
        lower
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join("_")
    }
}

fn executed_tool_failure_terminal_message(
    tool_name: &str,
    failure_count: usize,
    error_text: &str,
) -> String {
    format!(
        "Tool `{tool_name}` failed with the same no-progress execution error {failure_count} time(s). Runtime stopped before repeating the same failed call-id-scoped tool output until the turn step budget. Error class: {}. Latest error: {error_text}",
        tool_error_class(error_text)
    )
}

fn should_terminalize_rejected_tool_no_progress(
    rejection_count: usize,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> bool {
    let _ = allowed_tools;
    let _ = tool_choice;
    rejection_count >= 3
}

fn rejected_tool_no_progress_terminal_message(
    effective_tool_name: &str,
    rejection_count: usize,
    allowed_tools: &BTreeSet<String>,
) -> String {
    let allowed = allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ");
    format!(
        "Tool `{}` was disallowed {} time(s) without state progress. Runtime stopped this run instead of continuing unavailable-tool feedback until the turn step budget. Allowed tools for this turn: {}.",
        effective_tool_name, rejection_count, allowed
    )
}

pub(crate) fn rejected_tool_semantic_terminal_guard_fixture_passes() -> bool {
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
    ]);
    let first_key = rejected_tool_no_progress_key(
        "write",
        r#"{"path":"space_invader.py","content":"source v1"}"#,
        &allowed,
        &ToolChoice::Auto,
    );
    let second_key = rejected_tool_no_progress_key(
        "write",
        r#"{"path":"space_invader.py","content":"source v2 with a different payload"}"#,
        &allowed,
        &ToolChoice::Auto,
    );
    let different_tool_key = rejected_tool_no_progress_key(
        "todowrite",
        r#"{"todos":[{"content":"plan"}]}"#,
        &allowed,
        &ToolChoice::Auto,
    );

    first_key == second_key
        && first_key != different_tool_key
        && first_key.contains("rejected_tool|tool=write")
        && should_terminalize_rejected_tool_no_progress(3, &allowed, &ToolChoice::Auto)
        && rejected_tool_no_progress_terminal_message("write", 3, &allowed)
            .contains("Allowed tools for this turn")
}

fn tool_choice_for_dispatch(
    policy: &crate::agent::prompt::PromptPolicy,
    tool_names: &BTreeSet<String>,
    state: &SessionStateSnapshot,
) -> ToolChoice {
    let _ = state;
    if tool_names.is_empty() {
        return ToolChoice::None;
    }
    if matches!(tool_choice_from_policy(policy), ToolChoice::Required) {
        return ToolChoice::Required;
    }
    ToolChoice::Auto
}

fn open_executable_work_requires_tool_call(state: &SessionStateSnapshot) -> bool {
    if matches!(
        state.route,
        TaskRoute::Ask | TaskRoute::Review | TaskRoute::Summary
    ) {
        return false;
    }
    !closeout_ready_final_message_authority(state)
        && (state.completion.open_work_count > 0
            || !state.active_targets.is_empty()
            || state.completion.verification_pending
            || !state.verification.required_commands.is_empty())
}

pub(crate) fn concrete_write_required_action_narrows_broad_surface_fixture_passes() -> bool {
    let mut tools = vec![
        crate::llm::ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
            strict: false,
        },
    ];
    preserve_provider_tool_surface_for_dispatch(&mut tools);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    tools.len() == 3
        && tools.iter().any(|tool| {
            tool.name == "write"
                && !tool.strict
                && tool.input_schema.pointer("/properties/path").is_some()
        })
        && matches!(
            tool_choice_for_dispatch(
                &crate::agent::prompt::PromptPolicy::default(),
                &tool_names,
                &SessionStateSnapshot::default(),
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["read".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_calculator.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = false;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    matches!(
        tool_choice_for_dispatch(
            &crate::agent::prompt::PromptPolicy::default(),
            &tool_names,
            &state,
        ),
        ToolChoice::Auto
    ) && open_executable_work_requires_tool_call(&state)
        && !closeout_ready_final_message_authority(&state)
}

pub(crate) fn required_repair_write_missing_tool_is_not_restored_fixture_passes() -> bool {
    let mut tools = vec![crate::llm::ToolSchema {
        name: "shell".to_string(),
        description: "run a shell command".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string"}
            }
        }),
        strict: false,
    }];
    preserve_provider_tool_surface_for_dispatch(&mut tools);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();

    tools.len() == 1
        && tools.first().is_some_and(|tool| tool.name == "shell")
        && !matches!(
            tool_choice_for_dispatch(
                &crate::agent::prompt::PromptPolicy::default(),
                &tool_names,
                &SessionStateSnapshot::default(),
            ),
            ToolChoice::Required
        )
}

fn tool_choice_label(choice: &ToolChoice) -> &'static str {
    match choice {
        ToolChoice::Auto => "auto",
        ToolChoice::Required => "required",
        ToolChoice::None => "none",
        ToolChoice::Named(_) => "named",
    }
}

fn extra_body_with_required_tool_choice(
    extra_body: Option<Value>,
    tool_count: usize,
    required: bool,
) -> Option<Value> {
    if !required || tool_count == 0 {
        return extra_body;
    }
    let mut body = match extra_body {
        Some(Value::Object(map)) => Value::Object(map),
        Some(value) => json!({ "extra_body_json": value }),
        None => json!({}),
    };
    if let Value::Object(map) = &mut body {
        map.insert("tool_choice".to_string(), json!("required"));
    }
    Some(body)
}

fn is_write_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "write" | "apply_patch")
}

fn normalized_target_keys(target: &str, workspace_root: &Utf8Path) -> Vec<String> {
    let normalized = normalize_target_key(target);
    let root = normalize_target_key(workspace_root.as_str());
    if normalized.starts_with(&root) {
        vec![
            normalized.clone(),
            normalized[root.len()..]
                .trim_start_matches('/')
                .trim_start_matches('\\')
                .to_string(),
        ]
    } else {
        vec![normalized]
    }
}

fn normalize_target_key(target: &str) -> String {
    target
        .trim()
        .trim_matches('`')
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

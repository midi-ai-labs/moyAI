//! Thin agent loop boundary shared by CLI, TUI, and Desktop surfaces.

pub mod context_manager;
mod goal_steering;
pub mod mode;
pub mod step_context;
pub mod turn_context;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::{AccessMode, MultiAgentMode, ResolvedConfig};
use crate::context::context_window::ContextWindowTokenStatus;
use crate::context::world_state::WorldState;
use crate::error::AgentError;
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelContentPart,
    ModelMessage, ModelProfile, ModelToolCall, ToolSchema,
};
#[cfg(test)]
use crate::protocol::{ContentPart, HistoryItem};
use crate::protocol::{HistoryItemPayload, ModelResponseId, ProtocolEventStore, TurnId};
use crate::runtime::{
    ActiveSteerInput, LiveConfigOverrides, RunCancelOutcome, RunCancellationCause, RunControl,
    RunEventSink, SuccessCommitReservation,
};
use crate::session::{
    DurableTurnTerminal, FinishReason, RequestDiagnosticsPart, RequestMessageDiagnostic,
    RequestToolCallDiagnostic, RequestToolSchemaDiagnostic, RunConfigSnapshot, RunEvent,
    RunMetrics, RunSummary, SessionContext, SessionStatus, ThreadGoal, ThreadGoalStatus,
    TokenUsage, ToolCallId,
};
use crate::storage::{
    StoreBundle,
    session_repo::{
        AdmittedTerminalCommit, ModelResponseWrite, PendingToolCallWrite,
        RunAdmissionLeaseRenewalOutcome,
    },
};
use crate::tool::ToolResult;
use crate::tool::context::{RunMutationFence, ToolServices};
use crate::tool::registry::ToolRegistry;

const TOOL_CANCELLATION_CLEANUP_TIMEOUT: Duration =
    crate::tool::process::MANAGED_PROCESS_CLEANUP_GRACE;

async fn await_tool_cancellation_cleanup<T>(
    cleanup: impl Future<Output = T>,
    grace: Duration,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(grace, cleanup).await
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(
        &self,
        world_state: &WorldState,
        skills_snapshot: &crate::skill::SkillsSnapshot,
        turn: &turn_context::TurnContext,
        is_sub_agent: bool,
    ) -> String {
        let mut sections = vec![
            turn.policy.model.base_instructions.trim().to_string(),
            world_state.rendered.clone(),
            crate::skill::render_available_skills_from_snapshot(skills_snapshot),
        ];
        if let Some(instructions) = turn.mode.developer_instructions {
            sections.insert(1, instructions.trim().to_string());
        }
        if let Some(multi_agent_mode) = turn.multi_agent_mode {
            sections.push(
                match multi_agent_mode {
                    MultiAgentMode::ExplicitRequestOnly => {
                        include_str!("../../assets/prompts/multi_agent_explicit.md")
                    }
                    MultiAgentMode::Proactive => {
                        include_str!("../../assets/prompts/multi_agent_proactive.md")
                    }
                }
                .trim()
                .to_string(),
            );
            if is_sub_agent {
                sections.push(
                    include_str!("../../assets/prompts/sub_agent.md")
                        .trim()
                        .to_string(),
                );
            }
        }
        sections.join("\n\n")
    }
}

pub struct AgentRunRequest {
    pub session: SessionContext,
    pub turn: Arc<turn_context::TurnContext>,
    pub context: context_manager::ContextManager,
    pub config: ResolvedConfig,
    pub run_control: RunControl,
    pub live_config: Option<LiveConfigOverrides>,
    pub steer_rx: Option<UnboundedReceiver<ActiveSteerInput>>,
    pub agent_context: Option<crate::app::AgentRunContext>,
}

impl AgentRunRequest {
    fn cancel_token(&self) -> CancellationToken {
        self.run_control.token()
    }

    fn current_access_mode(&self) -> AccessMode {
        self.live_config
            .as_ref()
            .map(LiveConfigOverrides::access_mode)
            .unwrap_or(self.config.permissions.access_mode)
    }

    fn admission_id(&self) -> &str {
        &self.turn.admission_id
    }

    fn turn_id(&self) -> TurnId {
        self.turn.turn_id
    }

    fn model_profile(&self) -> ModelProfile {
        self.turn
            .policy
            .model
            .transport_profile(self.config.model.provider_metadata_mode)
    }

    fn model_name(&self) -> &str {
        &self.turn.policy.model.id
    }
}

#[derive(Clone)]
pub struct AgentLoop {
    llm: Arc<dyn LlmClient>,
    registry: ToolRegistry,
    store: StoreBundle,
    prompt_builder: PromptBuilder,
    tool_services: ToolServices,
    model_request_gate: Option<Arc<tokio::sync::Semaphore>>,
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
            model_request_gate: None,
        }
    }

    pub fn with_model_request_concurrency(mut self, max_concurrent_requests: usize) -> Self {
        self.model_request_gate = Some(Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_requests.max(1),
        )));
        self
    }

    pub async fn run(
        &self,
        mut request: AgentRunRequest,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        ensure_admission_active(&self.store, &request).await?;
        let repo = self.store.session_repo();

        let started_at = Instant::now();
        let mut seen_steer_ids = request
            .context
            .history_items()
            .iter()
            .filter_map(|item| {
                matches!(item.payload, HistoryItemPayload::SteerTurn { .. }).then_some(item.id)
            })
            .collect::<HashSet<_>>();
        let mut seen_agent_message_ids = request
            .context
            .history_items()
            .iter()
            .filter_map(|item| {
                matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                )
                .then_some(item.id)
            })
            .collect::<HashSet<_>>();
        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut model_request_count = 0usize;
        let mut tool_calls_by_name = BTreeMap::<String, usize>::new();
        let mut failed_tool_calls_by_name = BTreeMap::<String, usize>::new();
        let mut latest_usage: Option<TokenUsage> = None;
        let mut active_goal_id_for_turn: Option<String> = None;
        let mut last_model_response_id: Option<ModelResponseId> = None;
        let mut llm_turn =
            crate::llm::turn_session::LlmTurnSession::new(request.context.revision().as_str());

        let outcome: Result<RunSummary, AgentError> = async {
            loop {
                if request.run_control.is_cancelled() {
                    return self
                        .finish_for_run_control_cause(
                            &request,
                            last_model_response_id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }
                ensure_admission_active(&self.store, &request).await?;

                drain_pending_steers(
                    &self.store,
                    &mut request,
                    &mut seen_steer_ids,
                )
                .await?;
                drain_pending_agent_communications(
                    &self.store,
                    &request,
                    &mut seen_agent_message_ids,
                )?;

                let committed_history = self
                    .store
                    .protocol_event_store()
                    .list_history_items_for_session(request.session.session.id)?;
                let history_change = request.context.replace_committed_history(committed_history);
                llm_turn.update_history(
                    match history_change {
                        context_manager::HistoryChange::Unchanged => {
                            crate::llm::turn_session::HistoryUpdateKind::Unchanged
                        }
                        context_manager::HistoryChange::Appended => {
                            crate::llm::turn_session::HistoryUpdateKind::AppendOnly
                        }
                        context_manager::HistoryChange::Compacted => {
                            crate::llm::turn_session::HistoryUpdateKind::Compacted
                        }
                        context_manager::HistoryChange::Rewritten => {
                            crate::llm::turn_session::HistoryUpdateKind::Rewritten
                        }
                    },
                    request.context.revision().as_str(),
                );
                let mut step_config = request.config.clone();
                step_config.permissions.access_mode = request.current_access_mode();
                let step_registry = self.registry.with_config_overlays(&step_config);
                let skills = self
                    .tool_services
                    .skills
                    .snapshot_for_workspace(&request.session.workspace.root);
                let mut step = step_context::StepContext::capture(
                    Arc::clone(&request.turn),
                    &request.session.workspace,
                    &step_config,
                    skills,
                    &step_registry.available_tool_names(),
                );
                let tool_plan =
                    crate::tool::spec_plan::ToolSpecPlan::build(&step, &step_registry);
                step.refresh_world_state(
                    &request.session.workspace,
                    &step_config,
                    &tool_plan.tool_names(),
                );
                let messages = request
                    .context
                    .model_messages(
                        request
                            .turn
                            .policy
                            .model
                            .input_modalities
                            .contains(&crate::llm::model_policy::InputModality::Image),
                    );

                if let Some(agent) = request.agent_context.as_ref() {
                    agent.set_activity(format!("Preparing model request {}", model_request_count + 1));
                }

                let goal_for_request = self.goal_for_request(request.session.session.id).await?;
                if let Some(goal_for_request) = &goal_for_request {
                    active_goal_id_for_turn = Some(goal_for_request.goal_id.clone());
                }
                let (request_messages, has_transient_goal_steering) = messages_with_goal_steering(
                    &messages,
                    goal_for_request.as_ref().map(|goal| &goal.goal),
                );
                let mut prepared_request =
                    self.chat_request(&request, &step, &request_messages, &tool_plan)?;
                if has_transient_goal_steering {
                    // Goal steering is a request-local item whose budget text can change every
                    // round, so it never participates in persisted Responses cursor lineage.
                    llm_turn.invalidate_cursor();
                    prepared_request.chat_request.responses_continuation = None;
                } else {
                    llm_turn.prepare_request(
                        &mut prepared_request.chat_request,
                        request.context.revision().as_str(),
                    )?;
                }
                let context_status = ContextWindowTokenStatus::for_request(
                    &prepared_request.chat_request,
                    request.config.session.overflow_margin_tokens,
                );
                let should_compact = context_status.active_context_tokens
                    >= request.turn.policy.model.auto_compact_token_limit;
                if should_compact {
                    match self
                        .compact_context(
                            &mut request,
                            &prepared_request.chat_request,
                            &mut model_request_count,
                            sink,
                        )
                        .await
                    {
                        Ok(true) => {
                            llm_turn.invalidate_cursor();
                            continue;
                        }
                        Ok(false) if context_status.token_limit_reached => {
                            return Err(context_limit_error(&context_status));
                        }
                        Ok(false) => {}
                        Err(error) if context_status.token_limit_reached => return Err(error),
                        Err(error) => {
                            sink.emit(RunEvent::RecoverableRuntimeFeedback {
                                session_id: request.session.session.id,
                                message: format!(
                                    "semantic compaction failed without changing history; continuing below the hard context limit: {error}"
                                ),
                            })?;
                        }
                    }
                } else if context_status.token_limit_reached {
                    return Err(context_limit_error(&context_status));
                }
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
                renew_admission_lease(&self.store, &request).await?;
                model_request_count += 1;
                let model_response_id = ModelResponseId::new();
                let mut sent_request = prepared_request.chat_request.clone();
                let Some((mut response_result, mut collector)) = self
                    .execute_model_request(
                        &request,
                        sent_request.clone(),
                        model_response_id,
                        sink,
                    )
                    .await?
                else {
                    return self
                        .finish_for_run_control_cause(
                            &request,
                            last_model_response_id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                };
                if !has_transient_goal_steering && collector.is_empty() {
                    if let Err(error) = &response_result {
                        if let Some(retry) =
                            llm_turn.full_history_retry_after_rejection(&sent_request, error)
                        {
                            sent_request = retry;
                            sink.emit(RunEvent::ModelRequestPrepared {
                                session_id: request.session.session.id,
                                diagnostics: request_diagnostics(
                                    &sent_request,
                                    request.config.session.overflow_margin_tokens,
                                ),
                            })?;
                            renew_admission_lease(&self.store, &request).await?;
                            model_request_count += 1;
                            let Some((retry_result, retry_collector)) = self
                                .execute_model_request(
                                    &request,
                                    sent_request.clone(),
                                    model_response_id,
                                    sink,
                                )
                                .await?
                            else {
                                return self
                                    .finish_for_run_control_cause(
                                        &request,
                                        last_model_response_id,
                                        latest_usage.clone(),
                                        tool_call_count,
                                        failed_tool_count,
                                        change_count,
                                        model_request_count,
                                        tool_calls_by_name.clone(),
                                        failed_tool_calls_by_name.clone(),
                                        started_at,
                                        active_goal_id_for_turn.as_deref(),
                                        sink,
                                    )
                                    .await;
                            };
                            response_result = retry_result;
                            collector = retry_collector;
                        }
                    }
                }
                ensure_admission_active(&self.store, &request).await?;
                let response = match response_result {
                    Ok(response) => response,
                    Err(error) => {
                        if let Some(usage) = error.token_usage().cloned() {
                            latest_usage = Some(usage.clone());
                            if let Some(goal_for_request) = &goal_for_request {
                                self.store
                                    .session_repo()
                                    .account_thread_goal_usage_for_goal(
                                        request.session.session.id,
                                        goal_token_delta(Some(&usage)),
                                        Some(goal_for_request.goal_id.as_str()),
                                    )
                                    .await?;
                            }
                        }
                        return Err(error.into());
                    }
                };
                if !has_transient_goal_steering {
                    llm_turn.record_response(
                        &sent_request,
                        request.context.revision().as_str(),
                        response.response_id.clone(),
                    )?;
                }
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

                if response.finish_reason == FinishReason::Cancelled {
                    return self
                        .finish_for_run_control_cause(
                            &request,
                            last_model_response_id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }
                if request.run_control.is_cancelled() {
                    return self
                        .finish_for_run_control_cause(
                            &request,
                            last_model_response_id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }

                let has_tool_calls = !collector.tool_calls.is_empty();
                validate_provider_response_terminal(response.finish_reason, has_tool_calls)?;

                if !has_tool_calls && collector.text.trim().is_empty() {
                    return Err(AgentError::Message(format!(
                        "provider returned an empty final response with finish_reason={:?}",
                        response.finish_reason
                    )));
                }

                // Allocate stable sidecar IDs without parsing or normalizing provider payloads.
                // The raw assistant/tool-call transaction must become durable before any
                // transient execution derivation can run.
                let raw_tool_calls = std::mem::take(&mut collector.tool_calls)
                    .into_iter()
                    .map(|call| (ToolCallId::new(), call))
                    .collect::<Vec<_>>();
                let assistant_text = (!collector.text.is_empty()).then(|| collector.text.clone());
                let assistant_protocol_sequence_no = assistant_text
                    .as_ref()
                    .and_then(|_| sink.reserve_protocol_sequence_no());
                let tool_call_writes = raw_tool_calls
                    .iter()
                    .map(|(id, call)| PendingToolCallWrite {
                        id: *id,
                        model_call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        arguments_json: call.arguments_json.clone(),
                        protocol_sequence_no: sink.reserve_protocol_sequence_no(),
                    })
                    .collect();
                let committed_response_events = repo
                    .record_model_response_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        request.turn_id(),
                        ModelResponseWrite {
                            response_id: model_response_id,
                            assistant_text,
                            assistant_protocol_sequence_no,
                            tool_calls: tool_call_writes,
                        },
                    )
                    .await?;
                for event in committed_response_events {
                    sink.emit_committed(event)?;
                }
                // A terminal may only reference a response after the complete assistant/tool-call
                // bundle is durable. Provider transport success alone does not create canonical
                // response lineage (for example, output-limit and invalid-shape responses fail
                // before this transaction).
                last_model_response_id = Some(model_response_id);

                // ToolName routing, JSON decoding, and schema validation are transient
                // execution concerns. They intentionally happen only after the exact raw
                // provider response bundle above has committed successfully.
                let prepared_tool_calls = raw_tool_calls
                    .into_iter()
                    .map(|(id, call)| {
                        prepare_model_tool_call(id, call, tool_plan.model_visible_specs())
                    })
                    .collect::<Vec<_>>();

                if prepared_tool_calls.is_empty() {
                    if drain_pending_steers(
                        &self.store,
                        &mut request,
                        &mut seen_steer_ids,
                    )
                    .await?
                        > 0
                    {
                        continue;
                    }
                    if drain_pending_agent_communications(
                        &self.store,
                        &request,
                        &mut seen_agent_message_ids,
                    )? > 0
                    {
                        continue;
                    }
                    let terminal = DurableTurnTerminal {
                        status: crate::protocol::TurnTerminalStatus::Completed,
                        finish_reason: Some(response.finish_reason),
                        interruption_cause: None,
                        final_response_id: last_model_response_id,
                        summary: collector.text.trim().to_string(),
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
                    };
                    let event = RunEvent::TurnTerminal {
                        session_id: request.session.session.id,
                        terminal: Box::new(terminal.clone()),
                    };
                    let Some(success_commit) = request.run_control.begin_success_commit() else {
                        return self
                            .finish_for_run_control_cause(
                                &request,
                                last_model_response_id,
                                latest_usage.clone(),
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                model_request_count,
                                tool_calls_by_name.clone(),
                                failed_tool_calls_by_name.clone(),
                                started_at,
                                active_goal_id_for_turn.as_deref(),
                                sink,
                            )
                            .await;
                    };
                    let terminal_commit = match repo
                        .terminalize_admitted_turn_with_protocol_event(
                            request.session.session.id,
                            request.admission_id(),
                            &event,
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                            Some(seen_steer_ids.len()),
                            Some(seen_agent_message_ids.len()),
                            None,
                        )
                        .await
                    {
                        Ok(commit) => commit,
                        Err(error) => {
                            match self
                                .durable_terminal_summary(&request)
                                .await
                            {
                                Ok(Some(summary)) => {
                                    resolve_success_commit_from_durable_summary(
                                        success_commit,
                                        &summary,
                                    );
                                    return Ok(summary);
                                }
                                Ok(None) => {
                                    let message = format!(
                                        "success terminal commit failed without durable terminal evidence: {error}"
                                    );
                                    success_commit.abandon_with_cancellation(
                                        RunCancellationCause::Failure(message.clone()),
                                    );
                                    return Err(AgentError::Message(message));
                                }
                                Err(authority_error) => {
                                    let message = format!(
                                        "success terminal commit failed and durable ownership could not be read: {error}; authority read: {authority_error}"
                                    );
                                    success_commit.abandon_with_cancellation(
                                        RunCancellationCause::Failure(message.clone()),
                                    );
                                    return Err(AgentError::Message(message));
                                }
                            }
                        }
                    };
                    if let AdmittedTerminalCommit::UnseenSteer { expected, actual } =
                        terminal_commit
                    {
                        success_commit.release();
                        let drained = drain_pending_steers(
                            &self.store,
                            &mut request,
                            &mut seen_steer_ids,
                        )
                        .await?;
                        if drained > 0 {
                            continue;
                        }
                        return Err(AgentError::Message(format!(
                            "session {} stores {actual} accepted steer items while the loop observed {expected}, but the new input could not be loaded",
                            request.session.session.id,
                        )));
                    }
                    if let AdmittedTerminalCommit::UnseenAgentCommunication {
                        expected,
                        actual,
                    } = terminal_commit
                    {
                        success_commit.release();
                        let drained = drain_pending_agent_communications(
                            &self.store,
                            &request,
                            &mut seen_agent_message_ids,
                        )?;
                        if drained > 0 {
                            continue;
                        }
                        return Err(AgentError::Message(format!(
                            "session {} stores {actual} inter-agent communication items while the loop observed {expected}, but the new input could not be loaded",
                            request.session.session.id,
                        )));
                    }
                    if matches!(
                        terminal_commit,
                        AdmittedTerminalCommit::NotOwned
                            | AdmittedTerminalCommit::AlreadyTerminalizedBySameAdmission
                    ) {
                        let durable_summary = match self
                            .durable_terminal_summary(&request)
                            .await
                        {
                            Ok(summary) => summary,
                            Err(error) => {
                                let message = format!(
                                    "durable terminal ownership could not be resolved after a lost success commit: {error}"
                                );
                                success_commit.abandon_with_cancellation(
                                    RunCancellationCause::Failure(message.clone()),
                                );
                                return Err(AgentError::Message(message));
                            }
                        };
                        if let Some(summary) = durable_summary {
                            resolve_success_commit_from_durable_summary(
                                success_commit,
                                &summary,
                            );
                            return Ok(summary);
                        }
                        success_commit
                            .abandon_with_cancellation(RunCancellationCause::Superseded);
                        return Err(run_superseded_error(&request));
                    }
                    success_commit.seal();
                    sink.emit_committed(event)?;
                    return Ok(run_summary_from_terminal(&request, terminal));
                }

                for call in prepared_tool_calls {
                    if request.run_control.is_cancelled() {
                        return self
                            .finish_for_run_control_cause(
                                &request,
                                last_model_response_id,
                                latest_usage.clone(),
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                model_request_count,
                                tool_calls_by_name.clone(),
                                failed_tool_calls_by_name.clone(),
                                started_at,
                                active_goal_id_for_turn.as_deref(),
                                sink,
                            )
                            .await;
                    }
                    ensure_admission_active(&self.store, &request).await?;
                    if let Some(agent) = request.agent_context.as_ref() {
                        agent.set_activity(format!("Running {}", call.call.tool_name));
                    }
                    tool_call_count += 1;
                    *tool_calls_by_name
                        .entry(call.call.tool_name.clone())
                        .or_default() += 1;
                    let tool_output = self
                        .handle_tool_call(
                            tool_plan.router(),
                            &request,
                            call.clone(),
                            prompt,
                            sink,
                            &mut model_request_count,
                        )
                        .await?;
                    record_tool_dispatch_failure(
                        &tool_output,
                        &call.call.tool_name,
                        &mut failed_tool_count,
                        &mut failed_tool_calls_by_name,
                    );
                    let (_result_text, tool_change_count) = match tool_output {
                        ToolDispatchOutcome::Completed {
                            result_text,
                            change_count,
                        } => (result_text, change_count),
                        ToolDispatchOutcome::Declined { result_text } => (result_text, 0),
                        ToolDispatchOutcome::Failed { result_text } => {
                            if let Some(RunCancellationCause::Failure(message)) =
                                request.run_control.cause()
                            {
                                return Err(AgentError::Message(message));
                            }
                            (result_text, 0)
                        }
                        ToolDispatchOutcome::Interrupted {
                            change_count: interrupted_change_count,
                        } => {
                            change_count += interrupted_change_count;
                            return self
                                .finish_for_run_control_cause(
                                    &request,
                                    last_model_response_id,
                                    latest_usage.clone(),
                                    tool_call_count,
                                    failed_tool_count,
                                    change_count,
                                    model_request_count,
                                    tool_calls_by_name.clone(),
                                    failed_tool_calls_by_name.clone(),
                                    started_at,
                                    active_goal_id_for_turn.as_deref(),
                                    sink,
                                )
                                .await;
                        }
                    };
                    ensure_admission_active(&self.store, &request).await?;
                    change_count += tool_change_count;
                }
            }
        }
        .await;

        match outcome {
            Ok(summary) => Ok(summary),
            Err(error) => {
                if matches!(&error, AgentError::RunSuperseded { .. }) {
                    if let Some(summary) = self.durable_terminal_summary(&request).await? {
                        return Ok(summary);
                    }
                    return Err(error);
                }
                let owned_status = repo
                    .admitted_run_status(
                        request.session.session.id,
                        &request.turn.admission_id,
                        request.turn.turn_id,
                    )
                    .await?;
                if owned_status != Some(SessionStatus::Running) {
                    request.run_control.supersede();
                    return Err(run_superseded_error(&request));
                }
                if request.run_control.is_cancelled() {
                    return self
                        .finish_for_run_control_cause(
                            &request,
                            last_model_response_id,
                            latest_usage.clone(),
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            model_request_count,
                            tool_calls_by_name.clone(),
                            failed_tool_calls_by_name.clone(),
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }
                let failure_message = error.to_string();
                if request
                    .run_control
                    .request_cancel(RunCancellationCause::Failure(failure_message.clone()))
                    != RunCancelOutcome::Applied
                {
                    if request.run_control.cause().is_some() {
                        return self
                            .finish_for_run_control_cause(
                                &request,
                                last_model_response_id,
                                latest_usage.clone(),
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                model_request_count,
                                tool_calls_by_name.clone(),
                                failed_tool_calls_by_name.clone(),
                                started_at,
                                active_goal_id_for_turn.as_deref(),
                                sink,
                            )
                            .await;
                    }
                    return Err(error);
                }
                let terminal = DurableTurnTerminal {
                    status: crate::protocol::TurnTerminalStatus::Failed,
                    finish_reason: Some(FinishReason::Error),
                    interruption_cause: None,
                    final_response_id: last_model_response_id,
                    summary: failure_message,
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
                };
                let terminal_commit = self
                    .commit_terminal(
                        &request,
                        &terminal,
                        active_goal_id_for_turn.as_deref(),
                        sink,
                    )
                    .await?;
                if terminal_commit != AdmittedTerminalCommit::Applied {
                    if let Some(summary) = self.durable_terminal_summary(&request).await? {
                        return Ok(summary);
                    }
                    request.run_control.supersede();
                    return Err(run_superseded_error(&request));
                }
                Err(error)
            }
        }
    }

    async fn execute_model_request(
        &self,
        request: &AgentRunRequest,
        chat_request: ChatRequest,
        response_id: ModelResponseId,
        sink: &mut dyn RunEventSink,
    ) -> Result<
        Option<(
            Result<LlmResponseSummary, crate::error::LlmError>,
            ResponseCollector,
        )>,
        AgentError,
    > {
        let request_gate = request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.model_request_gate.clone());
        let _model_request_permit = match request_gate {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                let cancel = request.cancel_token();
                tokio::pin!(acquire);
                tokio::select! {
                    permit = &mut acquire => Some(permit.map_err(|_| {
                        AgentError::Message("model request concurrency gate closed".to_string())
                    })?),
                    _ = cancel.cancelled() => None,
                }
            }
            None => None,
        };
        if request.run_control.is_cancelled() {
            return Ok(None);
        }

        let mut collector = StreamingResponseCollector::new(response_id, sink);
        let response = {
            let stream = self
                .llm
                .stream_chat(chat_request, request.cancel_token(), &mut collector);
            tokio::pin!(stream);
            loop {
                tokio::select! {
                    response = &mut stream => break Some(response),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if request.run_control.is_cancelled() {
                            break None;
                        }
                        ensure_admission_active(&self.store, request).await?;
                    }
                }
            }
        };
        Ok(response.map(|response| (response, collector.into_inner())))
    }

    fn chat_request(
        &self,
        request: &AgentRunRequest,
        step: &step_context::StepContext,
        messages: &[ModelMessage],
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
    ) -> Result<PreparedChatRequest, AgentError> {
        let provider_api_mode = request.turn.policy.provider.api_mode;
        let reasoning = request.turn.policy.reasoning.clone();
        let reasoning_capability = request.turn.policy.provider.reasoning;
        let model = request.model_profile();
        let chat_request = ChatRequest {
            model,
            base_url: request.config.model.base_url.clone(),
            system_prompt: self.prompt_builder.build(
                &step.world_state,
                &step.skills,
                &request.turn,
                request
                    .agent_context
                    .as_ref()
                    .is_some_and(crate::app::AgentRunContext::is_sub_agent),
            ),
            messages: messages.to_vec(),
            tools: tool_plan.model_visible_specs().to_vec(),
            provider_api_mode,
            reasoning,
            reasoning_capability,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: tool_plan.parallel_tool_calls(),
            timeout_ms: request.config.model.request_timeout_ms,
            stream_idle_timeout_ms: request.config.model.stream_idle_timeout_ms,
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
            world_state: step.world_state.clone(),
        })
    }

    async fn compact_context(
        &self,
        request: &mut AgentRunRequest,
        request_template: &ChatRequest,
        model_request_count: &mut usize,
        sink: &mut dyn RunEventSink,
    ) -> Result<bool, AgentError> {
        let supports_images = request
            .turn
            .policy
            .model
            .input_modalities
            .contains(&crate::llm::model_policy::InputModality::Image);
        let units = request.context.semantic_compaction_units();
        if units.is_empty() {
            return Ok(false);
        }
        let canonical_messages = request.context.model_messages(supports_images);
        let transient_messages = request_template
            .messages
            .get(canonical_messages.len()..)
            .unwrap_or_default()
            .to_vec();
        let active_ids = request.context.active_item_ids();
        let current_status = ContextWindowTokenStatus::for_request(
            request_template,
            request.config.session.overflow_margin_tokens,
        );
        let mut selected_units = Vec::new();
        let mut replacement_item_ids = Vec::new();
        let mut retained_messages = canonical_messages;
        for unit in units {
            replacement_item_ids.extend(unit.iter().copied());
            selected_units.push(unit);
            let selected = replacement_item_ids.iter().copied().collect::<HashSet<_>>();
            let retained_ids = active_ids
                .iter()
                .filter(|item_id| !selected.contains(item_id))
                .copied()
                .collect::<Vec<_>>();
            retained_messages = request
                .context
                .model_messages_for_items(&retained_ids, supports_images);
            let mut projected = request_template.clone();
            projected.messages = vec![semantic_compaction_message("[semantic summary]")];
            projected.messages.extend(retained_messages.clone());
            projected.messages.extend(transient_messages.clone());
            let projected_status = ContextWindowTokenStatus::for_request(
                &projected,
                request.config.session.overflow_margin_tokens,
            );
            if projected_status.active_context_tokens
                < request.turn.policy.model.auto_compact_token_limit
            {
                break;
            }
        }
        if replacement_item_ids.is_empty() {
            return Ok(false);
        }
        let segments = request
            .context
            .compaction_segments_for_units(&selected_units, supports_images);
        if segments.is_empty() {
            return Ok(false);
        }
        let summary = self
            .summarize_compaction_segments(
                request,
                request_template,
                segments,
                model_request_count,
                sink,
            )
            .await?;
        let mut post_compaction = request_template.clone();
        post_compaction.messages = vec![semantic_compaction_message(&summary)];
        post_compaction.messages.extend(retained_messages);
        post_compaction.messages.extend(transient_messages);
        let post_status = ContextWindowTokenStatus::for_request(
            &post_compaction,
            request.config.session.overflow_margin_tokens,
        );
        if post_status.active_context_tokens >= current_status.active_context_tokens {
            return Err(AgentError::Message(format!(
                "semantic compaction did not reduce the prepared request (before {} estimated tokens, after {}); canonical history was left unchanged",
                current_status.active_context_tokens, post_status.active_context_tokens
            )));
        }
        sink.emit(RunEvent::CompactionCompleted {
            summarized_messages: replacement_item_ids.len(),
            summary,
            replacement_item_ids,
        })?;
        Ok(true)
    }

    async fn summarize_compaction_segments(
        &self,
        request: &AgentRunRequest,
        request_template: &ChatRequest,
        mut segments: Vec<String>,
        model_request_count: &mut usize,
        sink: &mut dyn RunEventSink,
    ) -> Result<String, AgentError> {
        loop {
            let input_chars = segments.iter().map(|segment| segment.chars().count()).sum();
            let batches = build_compaction_batches(
                request_template,
                &segments,
                request.config.session.overflow_margin_tokens,
            )?;
            let mut summaries = Vec::with_capacity(batches.len());
            for batch in batches {
                summaries.push(
                    self.run_compaction_request(
                        request,
                        request_template,
                        batch,
                        model_request_count,
                        sink,
                    )
                    .await?,
                );
            }
            if summaries.len() == 1 {
                return Ok(summaries.remove(0));
            }
            let output_chars = summaries
                .iter()
                .map(|summary| summary.chars().count())
                .sum::<usize>();
            if output_chars >= input_chars {
                return Err(AgentError::Message(format!(
                    "semantic compaction map/reduce made no progress ({} input characters, {} summary characters); canonical history was left unchanged",
                    input_chars, output_chars
                )));
            }
            segments = summaries
                .into_iter()
                .enumerate()
                .map(|(index, summary)| format!("[partial summary {}]\n{}", index + 1, summary))
                .collect();
        }
    }

    async fn run_compaction_request(
        &self,
        request: &AgentRunRequest,
        request_template: &ChatRequest,
        content: String,
        model_request_count: &mut usize,
        sink: &mut dyn RunEventSink,
    ) -> Result<String, AgentError> {
        let compaction_request = compaction_request_with_content(request_template, content);
        sink.emit(RunEvent::ModelRequestPrepared {
            session_id: request.session.session.id,
            diagnostics: request_diagnostics(
                &compaction_request,
                request.config.session.overflow_margin_tokens,
            ),
        })?;
        renew_admission_lease(&self.store, request).await?;
        *model_request_count += 1;

        let request_gate = request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.model_request_gate.clone());
        let _permit = match request_gate {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                let cancel = request.cancel_token();
                tokio::pin!(acquire);
                tokio::select! {
                    permit = &mut acquire => Some(permit.map_err(|_| {
                        AgentError::Message("model request concurrency gate closed".to_string())
                    })?),
                    _ = cancel.cancelled() => return Err(AgentError::Message(
                        "semantic compaction was cancelled".to_string()
                    )),
                }
            }
            None => None,
        };

        let mut collector = ResponseCollector::default();
        let response = self
            .llm
            .stream_chat(compaction_request, request.cancel_token(), &mut collector)
            .await?;
        validate_provider_response_terminal(
            response.finish_reason,
            !collector.tool_calls.is_empty(),
        )?;
        if !collector.tool_calls.is_empty() {
            return Err(AgentError::Message(
                "semantic compaction returned tool calls instead of a summary".to_string(),
            ));
        }
        let summary = collector.text.trim().to_string();
        if summary.is_empty() {
            return Err(AgentError::Message(
                "semantic compaction returned an empty summary".to_string(),
            ));
        }
        if let Some(goal) = self.goal_for_request(request.session.session.id).await? {
            self.store
                .session_repo()
                .account_thread_goal_usage_for_goal(
                    request.session.session.id,
                    goal_token_delta(response.usage.as_ref()),
                    Some(goal.goal_id.as_str()),
                )
                .await?;
        }
        Ok(summary)
    }

    async fn handle_tool_call(
        &self,
        registry: &ToolRegistry,
        request: &AgentRunRequest,
        call: PreparedModelToolCall,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
        model_request_count: &mut usize,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let repo = self.store.session_repo();
        let PreparedModelToolCall {
            id: tool_call_id,
            call,
            tool: tool_name,
            arguments,
            validation_error,
        } = call;
        let metadata = serde_json::json!({});

        if let Some(error_text) = validation_error {
            let result_text = format!("invalid arguments for `{}`: {error_text}", call.tool_name);
            let Some(settlement) = request.run_control.begin_tool_settlement() else {
                return self
                    .settle_pending_tool_for_run_cause(
                        request,
                        tool_call_id,
                        tool_name,
                        metadata,
                        sink,
                    )
                    .await;
            };
            let Some(failed) = repo
                .fail_tool_call_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &result_text,
                    failed_tool_metadata(metadata),
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            else {
                drop(settlement);
                return tool_terminal_race_outcome(request);
            };
            drop(settlement);
            sink.emit_committed(failed)?;
            return Ok(ToolDispatchOutcome::Failed { result_text });
        }

        renew_admission_lease(&self.store, request).await?;
        let permission_review_context = permission_review_context(request);
        let model_request_gate = request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.model_request_gate.clone());
        let permission_reviewer_model = request.model_profile();
        let ctx = crate::tool::context::ToolContext {
            session: &request.session,
            workspace: &request.session.workspace,
            config: &request.config,
            live_config: request.live_config.clone(),
            tool_call_id,
            cancel: request.cancel_token(),
            run_control: request.run_control.clone(),
            run_mutation_fence: RunMutationFence::new(
                self.store.session_repo(),
                request.session.session.id,
                request.admission_id().to_string(),
                request.turn_id(),
                request.run_control.clone(),
            ),
            prompt,
            services: &self.tool_services,
            agent: request.agent_context.as_ref(),
            permission_reviewer_llm: self.llm.as_ref(),
            permission_reviewer_model: &permission_reviewer_model,
            permission_review_context: &permission_review_context,
            model_request_gate,
            model_request_count,
        };
        let mut cancellation_cleanup_diagnostic = None;
        let execution_result = {
            // Multi-agent mutations cross the in-memory tree and durable session/protocol
            // stores. Once their fenced operation has started, let that short local operation
            // reach a consistent commit or rollback even if this turn is cancelled meanwhile.
            // The ownership check below still prevents recording a result for a stale run.
            let settle_multi_agent_mutation = matches!(
                call.tool_name.as_str(),
                "spawn_agent" | "send_message" | "followup_task" | "interrupt_agent"
            );
            let execution = registry.execute(&call.tool_name, arguments, ctx);
            tokio::pin!(execution);
            loop {
                tokio::select! {
                    result = &mut execution => break result,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if request.run_control.is_cancelled() {
                            break match await_tool_cancellation_cleanup(
                                &mut execution,
                                TOOL_CANCELLATION_CLEANUP_TIMEOUT,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    let message = format!(
                                        "tool cancellation cleanup exceeded {} ms",
                                        TOOL_CANCELLATION_CLEANUP_TIMEOUT.as_millis()
                                    );
                                    cancellation_cleanup_diagnostic = Some(message.clone());
                                    Err(crate::error::ToolError::Message(message))
                                }
                            };
                        }
                        if !settle_multi_agent_mutation {
                            ensure_admission_active(&self.store, request).await?;
                        }
                    }
                }
            }
        };
        let metadata = cancellation_cleanup_diagnostic
            .as_deref()
            .map(|diagnostic| tool_cleanup_diagnostic_metadata(metadata.clone(), diagnostic))
            .unwrap_or(metadata);
        ensure_admission_active(&self.store, request).await?;
        match execution_result {
            Ok(result) => {
                let result_text = tool_result_text(&result);
                let change_count = result.recorded_changes.len();
                let metadata = merge_tool_metadata(metadata, &result);
                if request.run_control.cause().is_some() {
                    return self
                        .settle_executed_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            &result_text,
                            result,
                            sink,
                        )
                        .await;
                }
                let Some(settlement) = request.run_control.begin_tool_settlement() else {
                    return self
                        .settle_executed_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            &result_text,
                            result,
                            sink,
                        )
                        .await;
                };
                if result.change_summaries.is_empty() {
                    let Some(completed) = repo
                        .complete_tool_call_with_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(completed)?;
                } else {
                    let Some((completed, file_changes)) = repo
                        .complete_tool_call_with_file_changes_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            result.change_summaries,
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(completed)?;
                    sink.emit_committed(file_changes)?;
                }
                Ok(ToolDispatchOutcome::Completed {
                    result_text,
                    change_count,
                })
            }
            Err(mut error) => {
                let result_text = error.to_string();
                let permission_denial_settlement = match &mut error {
                    crate::error::ToolError::PermissionDenied { settlement } => settlement.take(),
                    _ => None,
                };
                if matches!(
                    &error,
                    crate::error::ToolError::PermissionDenied { .. }
                        | crate::error::ToolError::PermissionAborted
                ) {
                    let permission_denied =
                        matches!(&error, crate::error::ToolError::PermissionDenied { .. });
                    let permission_aborted =
                        matches!(&error, crate::error::ToolError::PermissionAborted);
                    let abort_origin_owns = permission_aborted
                        && matches!(
                            request.run_control.cause(),
                            Some(RunCancellationCause::Interruption(
                                crate::protocol::TurnInterruptionCause::ApprovalAborted
                            ))
                        );
                    let settlement = if permission_denied {
                        let Some(settlement) = permission_denial_settlement else {
                            return Err(crate::error::AgentError::Runtime(
                                crate::error::RuntimeError::Message(
                                    "accepted permission denial lost its durable settlement owner"
                                        .to_string(),
                                ),
                            ));
                        };
                        Some(settlement)
                    } else if abort_origin_owns {
                        None
                    } else {
                        let Some(settlement) = request.run_control.begin_tool_settlement() else {
                            return self
                                .settle_pending_tool_for_run_cause(
                                    request,
                                    tool_call_id,
                                    tool_name,
                                    metadata,
                                    sink,
                                )
                                .await;
                        };
                        Some(settlement)
                    };
                    let Some(declined) = repo
                        .settle_tool_call_without_execution_with_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            crate::session::ToolCallStatus::Declined,
                            &result_text,
                            metadata,
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(declined)?;
                    return Ok(if permission_aborted {
                        ToolDispatchOutcome::Interrupted { change_count: 0 }
                    } else {
                        ToolDispatchOutcome::Declined { result_text }
                    });
                }
                if request.run_control.cause().is_some() {
                    return self
                        .settle_pending_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            sink,
                        )
                        .await;
                }
                let Some(settlement) = request.run_control.begin_tool_settlement() else {
                    return self
                        .settle_pending_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            sink,
                        )
                        .await;
                };
                let Some(failed) = repo
                    .fail_tool_call_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        &result_text,
                        failed_tool_metadata(metadata),
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    drop(settlement);
                    return tool_terminal_race_outcome(request);
                };
                drop(settlement);
                sink.emit_committed(failed)?;
                Ok(ToolDispatchOutcome::Failed { result_text })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_pending_tool_for_run_cause(
        &self,
        request: &AgentRunRequest,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        metadata: Value,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let repo = self.store.session_repo();
        match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(interruption)) => {
                let Some(event) = repo
                    .settle_tool_call_without_execution_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        crate::session::ToolCallStatus::Cancelled,
                        interruption.legacy_reason(),
                        metadata,
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    return tool_terminal_race_outcome(request);
                };
                sink.emit_committed(event)?;
                Ok(ToolDispatchOutcome::Interrupted { change_count: 0 })
            }
            Some(RunCancellationCause::Failure(message)) => {
                let Some(event) = repo
                    .fail_tool_call_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        &message,
                        metadata,
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    return tool_terminal_race_outcome(request);
                };
                sink.emit_committed(event)?;
                Ok(ToolDispatchOutcome::Failed {
                    result_text: message,
                })
            }
            Some(RunCancellationCause::Superseded) | None => tool_terminal_race_outcome(request),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_executed_tool_for_run_cause(
        &self,
        request: &AgentRunRequest,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        metadata: Value,
        result_text: &str,
        result: ToolResult,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let change_count = result.recorded_changes.len();
        let (status, reason, interrupted) = match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(interruption)) => (
                crate::session::ToolCallStatus::Cancelled,
                interruption.legacy_reason().to_string(),
                true,
            ),
            Some(RunCancellationCause::Failure(message)) => {
                (crate::session::ToolCallStatus::Failed, message, false)
            }
            Some(RunCancellationCause::Superseded) | None => {
                return tool_terminal_race_outcome(request);
            }
        };
        let repo = self.store.session_repo();
        if result.change_summaries.is_empty() {
            let terminal = if status == crate::session::ToolCallStatus::Cancelled {
                repo.settle_tool_call_without_execution_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    status,
                    &reason,
                    metadata,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            } else {
                repo.fail_tool_call_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &reason,
                    metadata,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            };
            let Some(terminal) = terminal else {
                return tool_terminal_race_outcome(request);
            };
            sink.emit_committed(terminal)?;
        } else {
            let Some((terminal, file_changes)) = repo
                .settle_executed_tool_call_with_file_changes_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &result.title,
                    metadata,
                    result_text,
                    result.truncated_output_path.as_deref(),
                    status,
                    &reason,
                    result.change_summaries,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            else {
                return tool_terminal_race_outcome(request);
            };
            sink.emit_committed(terminal)?;
            sink.emit_committed(file_changes)?;
        }
        Ok(if interrupted {
            ToolDispatchOutcome::Interrupted { change_count }
        } else {
            ToolDispatchOutcome::Failed {
                result_text: reason,
            }
        })
    }

    async fn finish_for_run_control_cause(
        &self,
        request: &AgentRunRequest,
        final_response_id: Option<ModelResponseId>,
        usage: Option<TokenUsage>,
        tool_call_count: usize,
        failed_tool_count: usize,
        change_count: usize,
        model_request_count: usize,
        tool_calls_by_name: BTreeMap<String, usize>,
        failed_tool_calls_by_name: BTreeMap<String, usize>,
        started_at: Instant,
        expected_active_goal_id: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(cause)) => {
                let terminal = DurableTurnTerminal {
                    status: crate::protocol::TurnTerminalStatus::Interrupted,
                    finish_reason: Some(FinishReason::Cancelled),
                    interruption_cause: Some(cause),
                    final_response_id,
                    summary: cause.legacy_reason().to_string(),
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
                };
                let terminal_commit = self.commit_terminal(request, &terminal, None, sink).await?;
                if terminal_commit == AdmittedTerminalCommit::Applied {
                    return Ok(run_summary_from_terminal(request, terminal));
                }
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    return Ok(summary);
                }
                request.run_control.supersede();
                Err(run_superseded_error(request))
            }
            Some(RunCancellationCause::Superseded) => {
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    Ok(summary)
                } else {
                    Err(run_superseded_error(request))
                }
            }
            Some(RunCancellationCause::Failure(message)) => {
                let terminal = DurableTurnTerminal {
                    status: crate::protocol::TurnTerminalStatus::Failed,
                    finish_reason: Some(FinishReason::Error),
                    interruption_cause: None,
                    final_response_id,
                    summary: message,
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
                };
                let terminal_commit = self
                    .commit_terminal(request, &terminal, expected_active_goal_id, sink)
                    .await?;
                if terminal_commit == AdmittedTerminalCommit::Applied {
                    return Ok(run_summary_from_terminal(request, terminal));
                }
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    return Ok(summary);
                }
                request.run_control.supersede();
                Err(run_superseded_error(request))
            }
            None => Err(AgentError::Message(
                "run cancellation was observed without a classified cause".to_string(),
            )),
        }
    }

    async fn durable_terminal_summary(
        &self,
        request: &AgentRunRequest,
    ) -> Result<Option<RunSummary>, AgentError> {
        let terminal = self
            .store
            .protocol_event_store()
            .list_runtime_events(request.session.session.id, request.turn_id())?
            .into_iter()
            .rev()
            .find_map(|event| match event.msg {
                crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } => Some(*terminal),
                _ => None,
            });
        Ok(terminal.map(|terminal| run_summary_from_terminal(request, terminal)))
    }

    async fn commit_terminal(
        &self,
        request: &AgentRunRequest,
        terminal: &DurableTurnTerminal,
        expected_active_goal_id: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<AdmittedTerminalCommit, AgentError> {
        let event = RunEvent::TurnTerminal {
            session_id: request.session.session.id,
            terminal: Box::new(terminal.clone()),
        };
        let terminal_commit = self
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                request.session.session.id,
                request.admission_id(),
                &event,
                request.turn_id(),
                sink.reserve_protocol_sequence_no(),
                None,
                None,
                expected_active_goal_id,
            )
            .await?;
        if terminal_commit == AdmittedTerminalCommit::Applied {
            sink.emit_committed(event)?;
        }
        Ok(terminal_commit)
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
}

fn resolve_success_commit_from_durable_summary(
    reservation: SuccessCommitReservation,
    summary: &RunSummary,
) {
    match summary.status {
        SessionStatus::Completed => {
            reservation.seal();
        }
        SessionStatus::Cancelled => {
            let cause = summary.interruption_cause.map_or(
                RunCancellationCause::Superseded,
                RunCancellationCause::Interruption,
            );
            reservation.resolve_authoritative_cancellation(cause);
        }
        SessionStatus::Failed => {
            reservation.resolve_authoritative_cancellation(RunCancellationCause::Failure(
                "durable failure owner won the terminal commit".to_string(),
            ));
        }
        SessionStatus::Idle | SessionStatus::Running => {
            reservation.resolve_authoritative_cancellation(RunCancellationCause::Superseded);
        }
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

fn semantic_compaction_message(summary: &str) -> ModelMessage {
    ModelMessage::System {
        content: format!(
            "Earlier conversation context was compacted.\n{}",
            summary.trim()
        ),
    }
}

fn compaction_request_with_content(template: &ChatRequest, content: String) -> ChatRequest {
    let mut request = template.clone();
    request.system_prompt = include_str!("../../assets/prompts/compaction.md")
        .trim()
        .to_string();
    request.messages = vec![ModelMessage::User { content }];
    request.tools.clear();
    request.tool_choice = None;
    request.parallel_tool_calls = false;
    request.responses_continuation = None;
    request
}

fn compaction_content_fits(
    template: &ChatRequest,
    content: String,
    overflow_margin_tokens: usize,
) -> bool {
    !ContextWindowTokenStatus::for_request(
        &compaction_request_with_content(template, content),
        overflow_margin_tokens,
    )
    .token_limit_reached
}

fn build_compaction_batches(
    template: &ChatRequest,
    segments: &[String],
    overflow_margin_tokens: usize,
) -> Result<Vec<String>, AgentError> {
    if !compaction_content_fits(template, String::new(), overflow_margin_tokens) {
        return Err(AgentError::Message(
            "semantic compaction has no model input capacity after output and safety reservation; canonical history was left unchanged"
                .to_string(),
        ));
    }
    let mut batches = Vec::new();
    let mut current = String::new();
    for segment in segments.iter().filter(|segment| !segment.trim().is_empty()) {
        let candidate = if current.is_empty() {
            segment.clone()
        } else {
            format!("{current}\n\n---\n\n{segment}")
        };
        if compaction_content_fits(template, candidate.clone(), overflow_margin_tokens) {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            batches.push(std::mem::take(&mut current));
        }
        if compaction_content_fits(template, segment.clone(), overflow_margin_tokens) {
            current = segment.clone();
            continue;
        }
        batches.extend(split_compaction_segment(
            template,
            segment,
            overflow_margin_tokens,
        )?);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    if batches.is_empty() {
        return Err(AgentError::Message(
            "semantic compaction had no model-visible source content".to_string(),
        ));
    }
    Ok(batches)
}

fn split_compaction_segment(
    template: &ChatRequest,
    segment: &str,
    overflow_margin_tokens: usize,
) -> Result<Vec<String>, AgentError> {
    let characters = segment.chars().collect::<Vec<_>>();
    let mut offset = 0;
    let mut chunks = Vec::new();
    while offset < characters.len() {
        let mut low = 1;
        let mut high = characters.len() - offset;
        let mut best = 0;
        while low <= high {
            let mid = low + (high - low) / 2;
            let candidate = characters[offset..offset + mid].iter().collect::<String>();
            if compaction_content_fits(template, candidate, overflow_margin_tokens) {
                best = mid;
                low = mid + 1;
            } else {
                high = mid.saturating_sub(1);
            }
        }
        if best == 0 {
            return Err(AgentError::Message(
                "semantic compaction could not fit one source character in the configured context window"
                    .to_string(),
            ));
        }
        chunks.push(characters[offset..offset + best].iter().collect::<String>());
        offset += best;
    }
    Ok(chunks)
}

fn context_limit_error(status: &ContextWindowTokenStatus) -> AgentError {
    AgentError::Message(format!(
        "context window limit reached after semantic compaction was unavailable or insufficient (estimated active context {} tokens, context window {} tokens, reserved output and safety margin {} tokens); canonical history was left unchanged",
        status.active_context_tokens,
        status.full_context_window_limit,
        status
            .configured_max_output_tokens
            .saturating_add(status.overflow_margin_tokens)
    ))
}

fn messages_with_goal_steering(
    messages: &[ModelMessage],
    goal: Option<&ThreadGoal>,
) -> (Vec<ModelMessage>, bool) {
    let Some(goal) = goal else {
        return (messages.to_vec(), false);
    };
    let Some(steering) = goal_steering::steering_message_for_goal(goal) else {
        return (messages.to_vec(), false);
    };
    let mut request_messages = messages.to_vec();
    request_messages.push(steering);
    (request_messages, true)
}

async fn ensure_admission_active(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<(), AgentError> {
    let status = store
        .session_repo()
        .admitted_run_status(
            request.session.session.id,
            &request.turn.admission_id,
            request.turn.turn_id,
        )
        .await?;
    if status == Some(SessionStatus::Running) {
        return Ok(());
    }
    request.run_control.supersede();
    Err(run_superseded_error(request))
}

async fn renew_admission_lease(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<(), AgentError> {
    let renewed = store
        .session_repo()
        .renew_admitted_run_lease(
            request.session.session.id,
            &request.turn.admission_id,
            request.turn.turn_id,
        )
        .await?;
    if renewed == RunAdmissionLeaseRenewalOutcome::Renewed {
        Ok(())
    } else {
        request.run_control.supersede();
        Err(run_superseded_error(request))
    }
}

fn run_superseded_error(request: &AgentRunRequest) -> AgentError {
    AgentError::RunSuperseded {
        session_id: request.session.session.id,
        admission_id: request.admission_id().to_string(),
    }
}

fn tool_terminal_race_outcome(
    request: &AgentRunRequest,
) -> Result<ToolDispatchOutcome, AgentError> {
    match request.run_control.cause() {
        Some(RunCancellationCause::Interruption(_)) => {
            Ok(ToolDispatchOutcome::Interrupted { change_count: 0 })
        }
        Some(RunCancellationCause::Superseded) => Err(run_superseded_error(request)),
        Some(RunCancellationCause::Failure(message)) => Err(AgentError::Message(message)),
        None => {
            request.run_control.supersede();
            Err(run_superseded_error(request))
        }
    }
}

async fn drain_pending_steers(
    store: &StoreBundle,
    request: &mut AgentRunRequest,
    seen_steer_ids: &mut HashSet<crate::protocol::HistoryItemId>,
) -> Result<usize, AgentError> {
    ensure_admission_active(store, request).await?;
    let mut drained = 0;
    let turn_id = request.turn_id();
    if let Some(receiver) = request.steer_rx.as_mut() {
        while let Ok(input) = receiver.try_recv() {
            if input.steer.expected_turn_id != turn_id {
                request.run_control.supersede();
                return Err(run_superseded_error(request));
            }
            if seen_steer_ids.insert(input.history_item_id) {
                drained += 1;
            }
        }
    }
    let Some(stored_steers) = store
        .session_repo()
        .list_admitted_turn_steers(
            request.session.session.id,
            &request.turn.admission_id,
            request.turn.turn_id,
        )
        .await?
    else {
        request.run_control.supersede();
        return Err(run_superseded_error(request));
    };
    for item in stored_steers {
        if let HistoryItemPayload::SteerTurn { content, .. } = item.payload {
            if !seen_steer_ids.insert(item.id) {
                continue;
            }
            let _ = content;
            drained += 1;
        }
    }
    Ok(drained)
}

fn drain_pending_agent_communications(
    store: &StoreBundle,
    request: &AgentRunRequest,
    seen_message_ids: &mut HashSet<crate::protocol::HistoryItemId>,
) -> Result<usize, AgentError> {
    let Some(agent) = request.agent_context.as_ref() else {
        return Ok(0);
    };
    let _ = agent.drain_mailbox();
    let history_items = store
        .protocol_event_store()
        .list_history_items_for_session(request.session.session.id)?;
    let mut drained = 0;
    for item in history_items {
        let HistoryItemPayload::InterAgentCommunication { communication } = item.payload else {
            continue;
        };
        if !seen_message_ids.insert(item.id) {
            continue;
        }
        let _ = communication;
        drained += 1;
    }
    Ok(drained)
}

fn validate_provider_response_terminal(
    finish_reason: FinishReason,
    has_tool_calls: bool,
) -> Result<(), AgentError> {
    match (finish_reason, has_tool_calls) {
        (FinishReason::Length, _) => Err(AgentError::ProviderOutputLimit),
        (FinishReason::Error, _) => Err(AgentError::ProviderFinishError),
        (FinishReason::ToolCall, false) | (FinishReason::Stop, true) => {
            Err(AgentError::ProviderFinishShape {
                finish_reason,
                has_tool_calls,
            })
        }
        (FinishReason::Stop, false)
        | (FinishReason::ToolCall, true)
        | (FinishReason::Cancelled, _) => Ok(()),
    }
}

#[derive(Debug, Clone)]
struct PreparedModelToolCall {
    id: ToolCallId,
    call: ModelToolCall,
    tool: crate::tool::ToolName,
    arguments: Value,
    validation_error: Option<String>,
}

fn prepare_model_tool_call(
    id: ToolCallId,
    call: ModelToolCall,
    schemas: &[ToolSchema],
) -> PreparedModelToolCall {
    let tool = crate::tool::ToolName::parse(&call.tool_name);
    let parsed_arguments = parse_tool_arguments(&call.arguments_json)
        .and_then(|value| validate_shallow_schema(&call.tool_name, value, schemas));
    let (arguments, validation_error) = match parsed_arguments {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error.to_string())),
    };
    PreparedModelToolCall {
        id,
        call,
        tool,
        arguments,
        validation_error,
    }
}

enum ToolDispatchOutcome {
    Completed {
        result_text: String,
        change_count: usize,
    },
    Declined {
        result_text: String,
    },
    Failed {
        result_text: String,
    },
    Interrupted {
        change_count: usize,
    },
}

fn record_tool_dispatch_failure(
    outcome: &ToolDispatchOutcome,
    tool_name: &str,
    failed_tool_count: &mut usize,
    failed_tool_calls_by_name: &mut BTreeMap<String, usize>,
) {
    if !matches!(outcome, ToolDispatchOutcome::Failed { .. }) {
        return;
    }
    *failed_tool_count += 1;
    *failed_tool_calls_by_name
        .entry(tool_name.to_string())
        .or_default() += 1;
}

#[derive(Default)]
struct ResponseCollector {
    saw_event: bool,
    text: String,
    tool_calls: Vec<ModelToolCall>,
    tool_call_order: Vec<String>,
    tool_call_args: HashMap<String, String>,
    tool_call_names: HashMap<String, String>,
}

impl LlmEventSink for ResponseCollector {
    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        self.saw_event = true;
        match event {
            LlmEvent::TextDelta(delta) => self.text.push_str(&delta),
            LlmEvent::ReasoningSummaryDelta(_) => {}
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
    response_id: ModelResponseId,
    sink: &'a mut dyn RunEventSink,
}

impl<'a> StreamingResponseCollector<'a> {
    fn new(response_id: ModelResponseId, sink: &'a mut dyn RunEventSink) -> Self {
        Self {
            inner: ResponseCollector::default(),
            response_id,
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
                .emit_runtime_only(RunEvent::TextDelta {
                    response_id: self.response_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            LlmEvent::ReasoningSummaryDelta(delta) => self
                .sink
                .emit_runtime_only(RunEvent::ReasoningSummaryDelta {
                    response_id: self.response_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            _ => {}
        }
        self.inner.push(event)
    }
}

impl ResponseCollector {
    fn is_empty(&self) -> bool {
        !self.saw_event
    }

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

fn run_summary_from_terminal(
    request: &AgentRunRequest,
    terminal: DurableTurnTerminal,
) -> RunSummary {
    RunSummary {
        session_id: request.session.session.id,
        turn_id: Some(request.turn_id()),
        final_response_id: terminal.final_response_id,
        status: terminal.status.as_session_status(),
        finish_reason: terminal.finish_reason,
        interruption_cause: terminal.interruption_cause,
        tool_call_count: terminal.tool_call_count,
        failed_tool_count: terminal.failed_tool_count,
        change_count: terminal.change_count,
        metrics: terminal.metrics,
    }
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
            model: request.model_name().to_string(),
            base_url: request.config.model.base_url.clone(),
            access_mode: request.current_access_mode().as_str().to_string(),
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
                ModelContentPart::Image { data_base64, .. } => {
                    base64::engine::general_purpose::STANDARD
                        .decode(data_base64)
                        .ok()
                        .map(|bytes| bytes.len() as u64)
                }
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

fn permission_review_context(request: &AgentRunRequest) -> String {
    const MAX_ITEMS: usize = 20;
    const MAX_ITEM_CHARS: usize = 4_000;
    const MAX_CONTEXT_CHARS: usize = 16_000;

    let mut rendered = Vec::new();
    let mut remaining = MAX_CONTEXT_CHARS;
    for item in request.context.history_items().iter().rev().take(MAX_ITEMS) {
        if remaining == 0 {
            break;
        }
        let value = serde_json::to_string(&item.payload)
            .unwrap_or_else(|error| format!("{{\"serialization_error\":\"{error}\"}}"));
        let clipped = clip_chars(&value, MAX_ITEM_CHARS.min(remaining));
        remaining = remaining.saturating_sub(clipped.chars().count());
        rendered.push(clipped);
    }
    rendered.reverse();
    if rendered.is_empty() {
        "No prior task context is available.".to_string()
    } else {
        rendered.join("\n")
    }
}

fn clip_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let mut clipped = value
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        clipped.push('…');
        clipped
    }
}

#[cfg(test)]
fn messages_from_history(history_items: &[HistoryItem]) -> Vec<ModelMessage> {
    context_manager::ContextManager::rehydrate(history_items.to_vec()).model_messages(true)
}

#[cfg(test)]
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

fn failed_tool_metadata(mut metadata: Value) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert("success".to_string(), Value::Bool(false));
    }
    metadata
}

fn tool_cleanup_diagnostic_metadata(mut metadata: Value, diagnostic: &str) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "cancellation_cleanup_error".to_string(),
            Value::String(diagnostic.to_string()),
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::config::{AccessMode, ResolvedConfig};
    use crate::error::LlmError;
    use crate::llm::LlmResponseSummary;
    use crate::protocol::{
        ContentPart, HistoryItem, ProtocolEventStore, ToolLifecycleStatus, TurnTerminalStatus,
        UserInputItem, UserTurn,
    };
    use crate::runtime::SystemClock;
    use crate::session::{
        ChangeRepository, ProjectRepository, PromptDispatchPart, SessionRepository,
        SessionSelector, SessionStartRequest, ToolCallId,
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
                response_id: None,
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

    fn has_terminal_status(event: &RunEvent, status: TurnTerminalStatus) -> bool {
        matches!(
            event,
            RunEvent::TurnTerminal { terminal, .. } if terminal.status == status
        )
    }

    #[test]
    fn compaction_batches_cover_one_giant_item_without_exceeding_the_model_window() {
        let template = ChatRequest {
            model: ModelProfile {
                name: "test".to_string(),
                context_window: 256,
                max_output_tokens: 32,
                provider_metadata_mode: crate::config::ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: crate::llm::ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            base_url: "http://localhost".to_string(),
            system_prompt: "runtime prompt".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            provider_api_mode: crate::config::model::ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: crate::config::model::ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 1,
            stream_idle_timeout_ms: 1,
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
        let source = "巨大な単一入力".repeat(1_000);

        let batches = build_compaction_batches(&template, std::slice::from_ref(&source), 8)
            .expect("split giant compaction source");

        assert!(batches.len() > 1);
        assert_eq!(batches.concat(), source);
        assert!(
            batches
                .into_iter()
                .all(|batch| compaction_content_fits(&template, batch, 8))
        );
    }

    fn has_terminal_cause(
        event: &RunEvent,
        status: TurnTerminalStatus,
        cause: crate::protocol::TurnInterruptionCause,
    ) -> bool {
        matches!(
            event,
            RunEvent::TurnTerminal { terminal, .. }
                if terminal.status == status && terminal.interruption_cause == Some(cause)
        )
    }

    fn canonical_tool_statuses(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
    ) -> Vec<ToolLifecycleStatus> {
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("canonical history")
            .into_iter()
            .filter_map(|item| match item.payload {
                HistoryItemPayload::ToolOutput { status, .. } => Some(status),
                _ => None,
            })
            .collect()
    }

    fn assert_canonical_tool_statuses(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
        expected: &[ToolLifecycleStatus],
    ) {
        let history = store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("canonical history");
        assert_eq!(
            canonical_tool_statuses(store, session_id),
            expected,
            "canonical history: {history:#?}"
        );

        let connection = rusqlite::Connection::open(&store.paths().database_path)
            .expect("open canonical tool sidecar");
        let mut statement = connection
            .prepare(
                "SELECT tool.status
                 FROM tool_calls AS tool
                 INNER JOIN protocol_history_items AS history
                    ON history.id = tool.history_item_id
                 WHERE history.session_id = ?1
                 ORDER BY tool.started_at_ms ASC, tool.id ASC",
            )
            .expect("prepare canonical tool sidecar query");
        let mut sidecar_statuses = statement
            .query_map([session_id.to_string()], |row| row.get::<_, String>(0))
            .expect("query canonical tool sidecar")
            .collect::<Result<Vec<_>, _>>()
            .expect("read canonical tool sidecar");
        let mut expected_statuses = expected
            .iter()
            .map(|status| match status {
                ToolLifecycleStatus::Pending => "pending",
                ToolLifecycleStatus::Running => "running",
                ToolLifecycleStatus::Completed => "completed",
                ToolLifecycleStatus::Declined => "declined",
                ToolLifecycleStatus::Cancelled => "cancelled",
                ToolLifecycleStatus::Failed => "failed",
            })
            .collect::<Vec<_>>();
        // Canonical history owns response order. The sidecar only owns execution
        // state, and calls started in the same millisecond have no stable ordering.
        sidecar_statuses.sort_unstable();
        expected_statuses.sort_unstable();
        assert_eq!(sidecar_statuses, expected_statuses);
    }

    fn canonical_runtime_has_tool_status(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
        expected: ToolLifecycleStatus,
    ) -> bool {
        store
            .protocol_event_store()
            .list_runtime_events_for_session(session_id)
            .expect("canonical runtime events")
            .into_iter()
            .any(|event| {
                matches!(
                    event.msg,
                    crate::protocol::RuntimeEventMsg::ToolLifecycle { envelope }
                        if envelope.status == expected
                )
            })
    }

    fn canonical_file_change_ids(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
    ) -> Vec<crate::session::ChangeId> {
        store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .expect("canonical history")
            .into_iter()
            .flat_map(|item| match item.payload {
                HistoryItemPayload::FileChange { change_ids, .. } => change_ids,
                _ => Vec::new(),
            })
            .collect()
    }

    struct DecisionPrompt {
        requests: Vec<crate::tool::PermissionRequest>,
        decision: crate::cli::ReviewDecision,
    }

    impl Default for DecisionPrompt {
        fn default() -> Self {
            Self {
                requests: Vec::new(),
                decision: crate::cli::ReviewDecision::Approved,
            }
        }
    }

    impl ConfirmationPrompt for DecisionPrompt {
        fn confirm(
            &mut self,
            request: &crate::tool::PermissionRequest,
        ) -> Result<crate::cli::ReviewDecision, crate::error::CliPromptError> {
            self.requests.push(request.clone());
            Ok(self.decision)
        }
    }

    #[derive(Clone, Copy)]
    enum TerminalRaceToolBehavior {
        PermissionAbortOrigin,
        ApprovalAbortObserver,
        LateInterruptedResult,
        LateFailedResult,
    }

    struct TerminalRaceTool {
        behavior: TerminalRaceToolBehavior,
        start_barrier: Option<Arc<tokio::sync::Barrier>>,
    }

    #[derive(Clone, Copy)]
    enum EffectAdmissionRaceOrder {
        StopBeforeAdmission,
        AdmissionBeforeStop,
    }

    struct EffectAdmissionRaceTool {
        order: EffectAdmissionRaceOrder,
        reached_boundary: Arc<tokio::sync::Barrier>,
        release_boundary: Arc<tokio::sync::Barrier>,
        side_effects: Arc<AtomicUsize>,
    }

    #[derive(Clone, Copy, Debug)]
    enum DeniedSettlementRaceOrder {
        ProducerBeforeSettlement,
        SettlementBeforeProducer,
    }

    #[derive(Clone, Copy, Debug)]
    enum DeniedSettlementTerminalProducer {
        Stop,
        Failure,
    }

    struct DeniedSettlementRaceTool {
        order: DeniedSettlementRaceOrder,
        reached_boundary: Arc<tokio::sync::Barrier>,
        release_boundary: Arc<tokio::sync::Barrier>,
    }

    struct CooperativeCleanupTool {
        started: Arc<tokio::sync::Barrier>,
        cleanup_completed: Arc<AtomicBool>,
    }

    #[async_trait(?Send)]
    impl crate::tool::registry::Tool for EffectAdmissionRaceTool {
        fn spec(&self) -> crate::tool::ToolSpec {
            crate::tool::ToolSpec {
                name: ToolName::Write,
                effect: crate::tool::ToolEffectPolicy::mutation(),
                description: "deterministic permission/effect admission race fixture",
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _raw_arguments: Value,
            mut ctx: crate::tool::context::ToolContext<'_>,
        ) -> Result<ToolResult, crate::error::ToolError> {
            let admission = ctx
                .confirm_if_needed(
                    crate::workspace::AccessKind::Edit,
                    "admit deterministic side effect".to_string(),
                    vec![ctx.workspace.root.join("effect-admission.txt")],
                    false,
                    Vec::new(),
                )
                .await?;
            match self.order {
                EffectAdmissionRaceOrder::StopBeforeAdmission => {
                    self.reached_boundary.wait().await;
                    self.release_boundary.wait().await;
                    admission.admit()?;
                    self.side_effects.fetch_add(1, Ordering::SeqCst);
                }
                EffectAdmissionRaceOrder::AdmissionBeforeStop => {
                    admission.admit()?;
                    self.side_effects.fetch_add(1, Ordering::SeqCst);
                    self.reached_boundary.wait().await;
                    self.release_boundary.wait().await;
                }
            }
            Ok(ToolResult {
                title: "effect admission fixture".to_string(),
                output_text: "effect boundary crossed".to_string(),
                metadata: serde_json::json!({"fixture": "effect_admission"}),
                truncated_output_path: None,
                recorded_changes: Vec::new(),
                change_summaries: Vec::new(),
            })
        }
    }

    #[async_trait(?Send)]
    impl crate::tool::registry::Tool for DeniedSettlementRaceTool {
        fn spec(&self) -> crate::tool::ToolSpec {
            crate::tool::ToolSpec {
                name: ToolName::Write,
                effect: crate::tool::ToolEffectPolicy::mutation(),
                description: "deterministic denial settlement race fixture",
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _raw_arguments: Value,
            ctx: crate::tool::context::ToolContext<'_>,
        ) -> Result<ToolResult, crate::error::ToolError> {
            let settlement = match self.order {
                DeniedSettlementRaceOrder::ProducerBeforeSettlement => {
                    self.reached_boundary.wait().await;
                    self.release_boundary.wait().await;
                    ctx.run_control
                        .begin_tool_settlement()
                        .ok_or(crate::error::ToolError::RunInterrupted)?
                }
                DeniedSettlementRaceOrder::SettlementBeforeProducer => {
                    let settlement = ctx
                        .run_control
                        .begin_tool_settlement()
                        .ok_or(crate::error::ToolError::RunInterrupted)?;
                    self.reached_boundary.wait().await;
                    self.release_boundary.wait().await;
                    settlement
                }
            };
            Err(crate::error::ToolError::PermissionDenied {
                settlement: Some(settlement),
            })
        }
    }

    #[async_trait(?Send)]
    impl crate::tool::registry::Tool for CooperativeCleanupTool {
        fn spec(&self) -> crate::tool::ToolSpec {
            crate::tool::ToolSpec {
                name: ToolName::Write,
                effect: crate::tool::ToolEffectPolicy::mutation(),
                description: "cooperative cancellation cleanup fixture",
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _raw_arguments: Value,
            ctx: crate::tool::context::ToolContext<'_>,
        ) -> Result<ToolResult, crate::error::ToolError> {
            self.started.wait().await;
            ctx.cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            self.cleanup_completed.store(true, Ordering::SeqCst);
            Err(crate::error::ToolError::RunInterrupted)
        }
    }

    #[async_trait(?Send)]
    impl crate::tool::registry::Tool for TerminalRaceTool {
        fn spec(&self) -> crate::tool::ToolSpec {
            crate::tool::ToolSpec {
                name: ToolName::Write,
                effect: crate::tool::ToolEffectPolicy::mutation(),
                description: "deterministic tool terminal race fixture",
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _raw_arguments: Value,
            ctx: crate::tool::context::ToolContext<'_>,
        ) -> Result<ToolResult, crate::error::ToolError> {
            if let Some(barrier) = &self.start_barrier {
                barrier.wait().await;
            }
            match self.behavior {
                TerminalRaceToolBehavior::PermissionAbortOrigin => {
                    ctx.run_control
                        .interrupt(crate::protocol::TurnInterruptionCause::ApprovalAborted);
                    Err(crate::error::ToolError::PermissionAborted)
                }
                TerminalRaceToolBehavior::ApprovalAbortObserver => {
                    ctx.run_control.token().cancelled().await;
                    Err(crate::error::ToolError::RunInterrupted)
                }
                TerminalRaceToolBehavior::LateInterruptedResult
                | TerminalRaceToolBehavior::LateFailedResult => {
                    let path = ctx.workspace.root.join(match self.behavior {
                        TerminalRaceToolBehavior::LateInterruptedResult => "late-interrupted.txt",
                        TerminalRaceToolBehavior::LateFailedResult => "late-failed.txt",
                        _ => unreachable!(),
                    });
                    let content = "side effect completed before classification\n";
                    ctx.run_mutation_fence.assert_owned().await?;
                    let effect_commit = ctx.run_mutation_fence.begin_effect_commit()?;
                    std::fs::write(&path, content)
                        .map_err(|error| crate::error::ToolError::Message(error.to_string()))?;
                    let stored_path =
                        crate::edit::path_for_change_storage(&path, &ctx.workspace.root);
                    let change = ctx.services.change_tracker.build_change(
                        ctx.tool_call_id,
                        None,
                        Some(&stored_path),
                        None,
                        Some(content),
                    )?;
                    ctx.services
                        .store
                        .change_repo()
                        .insert_changes(std::slice::from_ref(&change))
                        .await?;
                    drop(effect_commit);
                    match self.behavior {
                        TerminalRaceToolBehavior::LateInterruptedResult => {
                            ctx.run_control
                                .interrupt(crate::protocol::TurnInterruptionCause::ApprovalAborted);
                        }
                        TerminalRaceToolBehavior::LateFailedResult => {
                            ctx.run_control.fail("late tool failure");
                        }
                        _ => unreachable!(),
                    }
                    Ok(ToolResult {
                        title: "late tool result".to_string(),
                        output_text: "the tool future returned a result after classification"
                            .to_string(),
                        metadata: serde_json::json!({"fixture": "terminal_race"}),
                        truncated_output_path: None,
                        recorded_changes: vec![change.id],
                        change_summaries: vec![crate::edit::ChangeSummary {
                            change_id: change.id,
                            kind: change.kind,
                            path_before: change.path_before,
                            path_after: change.path_after,
                        }],
                    })
                }
            }
        }
    }

    #[test]
    fn tool_failure_metrics_count_only_typed_failed_outcomes() {
        let mut failed_tool_count = 0;
        let mut failed_tool_calls_by_name = BTreeMap::new();

        for outcome in [
            ToolDispatchOutcome::Completed {
                result_text: "completed".to_string(),
                change_count: 1,
            },
            ToolDispatchOutcome::Declined {
                result_text: "declined".to_string(),
            },
            ToolDispatchOutcome::Interrupted { change_count: 0 },
        ] {
            record_tool_dispatch_failure(
                &outcome,
                "write",
                &mut failed_tool_count,
                &mut failed_tool_calls_by_name,
            );
        }
        record_tool_dispatch_failure(
            &ToolDispatchOutcome::Failed {
                result_text: "operational failure".to_string(),
            },
            "shell",
            &mut failed_tool_count,
            &mut failed_tool_calls_by_name,
        );

        assert_eq!(failed_tool_count, 1);
        assert_eq!(failed_tool_calls_by_name.len(), 1);
        assert_eq!(failed_tool_calls_by_name.get("shell"), Some(&1));
        assert_eq!(failed_tool_calls_by_name.get("write"), None);
    }

    fn scripted_write_call(call_id: &str) -> ScriptedResponse {
        ScriptedResponse {
            events: vec![
                LlmEvent::ToolCallStart {
                    call_id: call_id.to_string(),
                    tool_name: "write".to_string(),
                },
                LlmEvent::ToolCallArgsDelta {
                    call_id: call_id.to_string(),
                    delta: "{}".to_string(),
                },
            ],
            finish_reason: FinishReason::ToolCall,
        }
    }

    #[tokio::test]
    async fn thin_loop_runs_scripted_provider_tool_turn() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::FullAccess;
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
                    finish_reason: FinishReason::ToolCall,
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
        assert_canonical_tool_statuses(
            &run.store,
            run.session_id,
            &[ToolLifecycleStatus::Completed],
        );
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
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
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
        assert!(summary.final_response_id.is_some());
    }

    #[tokio::test]
    async fn raw_tool_calls_commit_before_invalid_json_unknown_tool_and_schema_failures_settle() {
        let cases = [
            ("invalid_json", "read", "{not-json}"),
            ("unknown_tool", "unknown_provider_tool", "{}"),
            ("schema_mismatch", "read", r#"{"path":123}"#),
        ];

        for (label, tool_name, arguments_json) in cases {
            let mut config = ResolvedConfig::default();
            config.permissions.access_mode = AccessMode::FullAccess;
            let provider_call_id = format!("provider-{label}");
            let run = run_scripted(
                config,
                vec![
                    ScriptedResponse {
                        events: vec![
                            LlmEvent::ToolCallStart {
                                call_id: provider_call_id.clone(),
                                tool_name: tool_name.to_string(),
                            },
                            LlmEvent::ToolCallArgsDelta {
                                call_id: provider_call_id.clone(),
                                delta: arguments_json.to_string(),
                            },
                        ],
                        finish_reason: FinishReason::ToolCall,
                    },
                    ScriptedResponse {
                        events: vec![LlmEvent::TextDelta(
                            "continued after the failed tool call".to_string(),
                        )],
                        finish_reason: FinishReason::Stop,
                    },
                ],
            )
            .await
            .expect(label);
            let summary = run.summary.expect(label);
            assert_eq!(summary.status, SessionStatus::Completed, "case={label}");
            assert_eq!(summary.failed_tool_count, 1, "case={label}");

            let history = run
                .store
                .protocol_event_store()
                .list_history_items_for_session(run.session_id)
                .expect("canonical history");
            let (call_index, call_id) = history
                .iter()
                .enumerate()
                .find_map(|(index, item)| match &item.payload {
                    HistoryItemPayload::ToolCall {
                        call_id,
                        model_call_id,
                        tool_name: stored_tool_name,
                        arguments_json: stored_arguments_json,
                        ..
                    } if model_call_id == &provider_call_id => {
                        assert_eq!(stored_tool_name, tool_name, "case={label}");
                        assert_eq!(stored_arguments_json, arguments_json, "case={label}");
                        Some((index, *call_id))
                    }
                    _ => None,
                })
                .expect("raw tool call must commit");
            let (output_index, output_text) = history
                .iter()
                .enumerate()
                .find_map(|(index, item)| match &item.payload {
                    HistoryItemPayload::ToolOutput {
                        call_id: output_call_id,
                        status: ToolLifecycleStatus::Failed,
                        output_text,
                        ..
                    } if *output_call_id == call_id => Some((index, output_text)),
                    _ => None,
                })
                .expect("failed output must settle the committed raw call");
            assert!(call_index < output_index, "case={label}");
            assert!(!output_text.trim().is_empty(), "case={label}");
        }
    }

    #[tokio::test]
    async fn aborting_permission_stops_before_retry_or_sibling_tool_execution() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::Default;
        let run = run_scripted_with_options_and_decision(
            config,
            vec![ScriptedResponse {
                events: vec![
                    LlmEvent::ToolCallStart {
                        call_id: "call_1".to_string(),
                        tool_name: "write".to_string(),
                    },
                    LlmEvent::ToolCallArgsDelta {
                        call_id: "call_1".to_string(),
                        delta: r#"{"path":"first.txt","content":"must not be written\n"}"#
                            .to_string(),
                    },
                    LlmEvent::ToolCallStart {
                        call_id: "call_2".to_string(),
                        tool_name: "write".to_string(),
                    },
                    LlmEvent::ToolCallArgsDelta {
                        call_id: "call_2".to_string(),
                        delta: r#"{"path":"second.txt","content":"must not be written\n"}"#
                            .to_string(),
                    },
                ],
                finish_reason: FinishReason::ToolCall,
            }],
            None,
            None,
            None,
            crate::cli::ReviewDecision::Abort,
        )
        .await
        .expect("run setup");
        let summary = run.summary.expect("abort summary");
        let session = run
            .store
            .session_repo()
            .get_session(run.session_id)
            .await
            .expect("session");

        assert_eq!(summary.status, SessionStatus::Cancelled);
        assert_eq!(session.status, SessionStatus::Cancelled);
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.failed_tool_count, 0);
        assert_eq!(summary.metrics.model_request_count, 1);
        assert_eq!(run.requests.len(), 1);
        assert_eq!(run.confirmations.len(), 1);
        assert!(!run.root.join("first.txt").exists());
        assert!(!run.root.join("second.txt").exists());
        assert_canonical_tool_statuses(
            &run.store,
            run.session_id,
            &[
                ToolLifecycleStatus::Declined,
                ToolLifecycleStatus::Cancelled,
            ],
        );
        assert!(run.events.iter().any(|event| {
            has_terminal_cause(
                event,
                TurnTerminalStatus::Interrupted,
                crate::protocol::TurnInterruptionCause::ApprovalAborted,
            )
        }));
        assert!(
            !run.events
                .iter()
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
        );
    }

    #[tokio::test]
    async fn denied_permission_returns_one_tool_result_and_continues_the_same_turn() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::Default;
        let run = run_scripted_with_options_and_decision(
            config,
            vec![
                ScriptedResponse {
                    events: vec![
                        LlmEvent::ToolCallStart {
                            call_id: "call_denied".to_string(),
                            tool_name: "write".to_string(),
                        },
                        LlmEvent::ToolCallArgsDelta {
                            call_id: "call_denied".to_string(),
                            delta: r#"{"path":"denied.txt","content":"must not be written\n"}"#
                                .to_string(),
                        },
                    ],
                    finish_reason: FinishReason::ToolCall,
                },
                ScriptedResponse {
                    events: vec![LlmEvent::TextDelta(
                        "Understood; the action was not executed.".to_string(),
                    )],
                    finish_reason: FinishReason::Stop,
                },
            ],
            None,
            None,
            None,
            crate::cli::ReviewDecision::Denied,
        )
        .await
        .expect("run setup");
        let summary = run.summary.expect("denied run summary");

        assert_eq!(summary.status, SessionStatus::Completed);
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.failed_tool_count, 0);
        assert_eq!(summary.metrics.model_request_count, 2);
        assert_eq!(run.requests.len(), 2);
        assert_eq!(run.confirmations.len(), 1);
        assert!(!run.root.join("denied.txt").exists());
        assert!(run.requests[1].messages.iter().any(|message| {
            matches!(
                message,
                ModelMessage::Tool { result, .. }
                    if result.contains("permission denied by user")
            )
        }));
        assert!(
            run.events
                .iter()
                .any(|event| { matches!(event, RunEvent::ToolCallDeclined { .. }) })
        );
        assert!(
            !run.events
                .iter()
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Interrupted))
        );
        assert_canonical_tool_statuses(
            &run.store,
            run.session_id,
            &[ToolLifecycleStatus::Declined],
        );
    }

    #[tokio::test]
    async fn tool_effect_admission_makes_stop_first_zero_effect_and_admission_first_startable() {
        for (order, expected_effects) in [
            (EffectAdmissionRaceOrder::StopBeforeAdmission, 0),
            (EffectAdmissionRaceOrder::AdmissionBeforeStop, 1),
        ] {
            let mut config = ResolvedConfig::default();
            config.permissions.access_mode = AccessMode::Default;
            let run_control = RunControl::new();
            let reached_boundary = Arc::new(tokio::sync::Barrier::new(2));
            let release_boundary = Arc::new(tokio::sync::Barrier::new(2));
            let side_effects = Arc::new(AtomicUsize::new(0));
            let run = run_scripted_with_control_and_tool(
                config,
                vec![scripted_write_call("effect_admission")],
                run_control.clone(),
                Arc::new(EffectAdmissionRaceTool {
                    order,
                    reached_boundary: Arc::clone(&reached_boundary),
                    release_boundary: Arc::clone(&release_boundary),
                    side_effects: Arc::clone(&side_effects),
                }),
            );
            let classify = async {
                reached_boundary.wait().await;
                assert_eq!(
                    run_control.request_cancel(RunCancellationCause::Interruption(
                        crate::protocol::TurnInterruptionCause::UserStop
                    )),
                    match order {
                        EffectAdmissionRaceOrder::StopBeforeAdmission => RunCancelOutcome::Applied,
                        EffectAdmissionRaceOrder::AdmissionBeforeStop => RunCancelOutcome::Applied,
                    }
                );
                release_boundary.wait().await;
            };

            let (run, ()) = tokio::join!(run, classify);
            let run = run.expect("effect admission run");
            let summary = run.summary.expect("typed cancellation summary");
            assert_eq!(summary.status, SessionStatus::Cancelled);
            assert_eq!(
                summary.interruption_cause,
                Some(crate::protocol::TurnInterruptionCause::UserStop)
            );
            assert_eq!(side_effects.load(Ordering::SeqCst), expected_effects);
            assert!(
                run.events
                    .iter()
                    .any(|event| { matches!(event, RunEvent::ToolCallCancelled { .. }) })
            );
            assert!(
                !run.events
                    .iter()
                    .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
            );
        }
    }

    #[tokio::test]
    async fn tool_effect_admission_makes_failure_first_zero_effect_and_typed_failed_terminal() {
        for (order, expected_effects) in [
            (EffectAdmissionRaceOrder::StopBeforeAdmission, 0),
            (EffectAdmissionRaceOrder::AdmissionBeforeStop, 1),
        ] {
            let mut config = ResolvedConfig::default();
            config.permissions.access_mode = AccessMode::Default;
            let run_control = RunControl::new();
            let reached_boundary = Arc::new(tokio::sync::Barrier::new(2));
            let release_boundary = Arc::new(tokio::sync::Barrier::new(2));
            let side_effects = Arc::new(AtomicUsize::new(0));
            let run = run_scripted_with_control_and_tool(
                config,
                vec![scripted_write_call("effect_admission_failure")],
                run_control.clone(),
                Arc::new(EffectAdmissionRaceTool {
                    order,
                    reached_boundary: Arc::clone(&reached_boundary),
                    release_boundary: Arc::clone(&release_boundary),
                    side_effects: Arc::clone(&side_effects),
                }),
            );
            let classify = async {
                reached_boundary.wait().await;
                assert_eq!(
                    run_control.request_cancel(RunCancellationCause::Failure(
                        "provider failed at the effect boundary".to_string()
                    )),
                    RunCancelOutcome::Applied
                );
                release_boundary.wait().await;
            };

            let (run, ()) = tokio::join!(run, classify);
            let run = run.expect("effect admission failure run");
            let summary = run.summary.as_ref().expect("typed failure summary");
            assert_eq!(summary.status, SessionStatus::Failed);
            assert_eq!(summary.interruption_cause, None);
            assert_eq!(
                run.store
                    .session_repo()
                    .get_session(run.session_id)
                    .await
                    .expect("durable failed session")
                    .status,
                SessionStatus::Failed
            );
            assert_eq!(side_effects.load(Ordering::SeqCst), expected_effects);
            assert_eq!(run.requests.len(), 1, "terminal failure must stop replay");
            assert!(
                run.events
                    .iter()
                    .any(|event| matches!(event, RunEvent::ToolCallFailed { .. }))
            );
            assert!(
                run.events
                    .iter()
                    .any(|event| has_terminal_status(event, TurnTerminalStatus::Failed))
            );
            assert!(
                !run.events
                    .iter()
                    .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
            );
        }
    }

    #[tokio::test]
    async fn permission_denial_settlement_has_exact_order_against_stop_and_failure() {
        for order in [
            DeniedSettlementRaceOrder::ProducerBeforeSettlement,
            DeniedSettlementRaceOrder::SettlementBeforeProducer,
        ] {
            for producer in [
                DeniedSettlementTerminalProducer::Stop,
                DeniedSettlementTerminalProducer::Failure,
            ] {
                let config = ResolvedConfig::default();
                let run_control = RunControl::new();
                let reached_boundary = Arc::new(tokio::sync::Barrier::new(2));
                let release_boundary = Arc::new(tokio::sync::Barrier::new(2));
                let run = run_scripted_with_control_and_tool(
                    config,
                    vec![scripted_write_call("denial_settlement_race")],
                    run_control.clone(),
                    Arc::new(DeniedSettlementRaceTool {
                        order,
                        reached_boundary: Arc::clone(&reached_boundary),
                        release_boundary: Arc::clone(&release_boundary),
                    }),
                );
                let classify = async {
                    reached_boundary.wait().await;
                    let cause = match producer {
                        DeniedSettlementTerminalProducer::Stop => {
                            RunCancellationCause::Interruption(
                                crate::protocol::TurnInterruptionCause::UserStop,
                            )
                        }
                        DeniedSettlementTerminalProducer::Failure => RunCancellationCause::Failure(
                            "provider failed during denial settlement".to_string(),
                        ),
                    };
                    let outcome = run_control.request_cancel(cause);
                    match order {
                        DeniedSettlementRaceOrder::ProducerBeforeSettlement => {
                            assert_eq!(outcome, RunCancelOutcome::Applied)
                        }
                        DeniedSettlementRaceOrder::SettlementBeforeProducer => {
                            assert!(matches!(outcome, RunCancelOutcome::Deferred(_)))
                        }
                    }
                    release_boundary.wait().await;
                };

                let (run, ()) = tokio::join!(run, classify);
                let run = run.expect("denial settlement race run");
                let terminal_status = match run.summary.as_ref() {
                    Ok(summary) => summary.status,
                    Err(error) => {
                        assert!(matches!(
                            producer,
                            DeniedSettlementTerminalProducer::Failure
                        ));
                        assert!(
                            error
                                .to_string()
                                .contains("provider failed during denial settlement")
                        );
                        run.store
                            .session_repo()
                            .get_session(run.session_id)
                            .await
                            .expect("durable failed session")
                            .status
                    }
                };
                assert_eq!(run.requests.len(), 1, "terminal owner must stop replay");
                let declined_index = run
                    .events
                    .iter()
                    .position(|event| matches!(event, RunEvent::ToolCallDeclined { .. }));
                let terminal_index = run.events.iter().position(|event| match producer {
                    DeniedSettlementTerminalProducer::Stop => {
                        has_terminal_status(event, TurnTerminalStatus::Interrupted)
                    }
                    DeniedSettlementTerminalProducer::Failure => {
                        has_terminal_status(event, TurnTerminalStatus::Failed)
                    }
                });
                let terminal_index = terminal_index.expect("typed session terminal event");

                match (order, producer) {
                    (
                        DeniedSettlementRaceOrder::ProducerBeforeSettlement,
                        DeniedSettlementTerminalProducer::Stop,
                    ) => {
                        assert_eq!(terminal_status, SessionStatus::Cancelled);
                        assert!(declined_index.is_none());
                        assert!(
                            run.events
                                .iter()
                                .any(|event| matches!(event, RunEvent::ToolCallCancelled { .. }))
                        );
                    }
                    (
                        DeniedSettlementRaceOrder::ProducerBeforeSettlement,
                        DeniedSettlementTerminalProducer::Failure,
                    ) => {
                        assert_eq!(terminal_status, SessionStatus::Failed);
                        assert!(declined_index.is_none());
                        assert!(
                            run.events
                                .iter()
                                .any(|event| matches!(event, RunEvent::ToolCallFailed { .. }))
                        );
                    }
                    (DeniedSettlementRaceOrder::SettlementBeforeProducer, _) => {
                        let declined_index = declined_index.expect("durable denied settlement");
                        assert!(declined_index < terminal_index);
                        assert_eq!(
                            terminal_status,
                            match producer {
                                DeniedSettlementTerminalProducer::Stop => SessionStatus::Cancelled,
                                DeniedSettlementTerminalProducer::Failure => SessionStatus::Failed,
                            }
                        );
                    }
                }
                assert!(
                    !run.events
                        .iter()
                        .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
                );
            }
        }
    }

    #[tokio::test]
    async fn cancellation_awaits_bounded_cooperative_tool_cleanup_before_settlement() {
        let config = ResolvedConfig::default();
        let run_control = RunControl::new();
        let started = Arc::new(tokio::sync::Barrier::new(2));
        let cleanup_completed = Arc::new(AtomicBool::new(false));
        let run = run_scripted_with_control_and_tool(
            config,
            vec![scripted_write_call("cooperative_cleanup")],
            run_control.clone(),
            Arc::new(CooperativeCleanupTool {
                started: Arc::clone(&started),
                cleanup_completed: Arc::clone(&cleanup_completed),
            }),
        );
        let cancel = async {
            started.wait().await;
            assert!(run_control.interrupt(crate::protocol::TurnInterruptionCause::UserStop));
        };

        let (run, ()) = tokio::join!(run, cancel);
        let run = run.expect("cooperative cleanup run");
        let summary = run.summary.expect("cancelled summary");
        assert_eq!(summary.status, SessionStatus::Cancelled);
        assert!(cleanup_completed.load(Ordering::SeqCst));
        assert_eq!(run.requests.len(), 1);
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, RunEvent::ToolCallCancelled { .. }))
        );
    }

    #[tokio::test]
    async fn non_cooperative_tool_cleanup_obeys_the_injected_deadline() {
        let started_at = Instant::now();
        let result = await_tool_cancellation_cleanup(
            std::future::pending::<()>(),
            Duration::from_millis(10),
        )
        .await;

        assert!(result.is_err());
        assert!(started_at.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn permission_abort_origin_is_declined_while_same_root_observer_is_cancelled() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::FullAccess;
        let run_control = RunControl::new();
        let start_barrier = Arc::new(tokio::sync::Barrier::new(2));
        let origin = run_scripted_with_control_and_tool(
            config.clone(),
            vec![scripted_write_call("abort_origin")],
            run_control.clone(),
            Arc::new(TerminalRaceTool {
                behavior: TerminalRaceToolBehavior::PermissionAbortOrigin,
                start_barrier: Some(Arc::clone(&start_barrier)),
            }),
        );
        let observer = run_scripted_with_control_and_tool(
            config,
            vec![scripted_write_call("abort_observer")],
            run_control,
            Arc::new(TerminalRaceTool {
                behavior: TerminalRaceToolBehavior::ApprovalAbortObserver,
                start_barrier: Some(start_barrier),
            }),
        );
        let (origin, observer) = tokio::join!(origin, observer);
        let origin = origin.expect("origin run setup");
        let observer = observer.expect("observer run setup");

        assert_eq!(
            origin.summary.expect("origin summary").status,
            SessionStatus::Cancelled
        );
        assert_eq!(
            observer.summary.expect("observer summary").status,
            SessionStatus::Cancelled
        );
        assert!(
            origin
                .events
                .iter()
                .any(|event| { matches!(event, RunEvent::ToolCallDeclined { .. }) })
        );
        assert!(
            !origin
                .events
                .iter()
                .any(|event| { matches!(event, RunEvent::ToolCallCancelled { .. }) })
        );
        assert!(
            observer
                .events
                .iter()
                .any(|event| { matches!(event, RunEvent::ToolCallCancelled { .. }) })
        );
        assert!(
            !observer
                .events
                .iter()
                .any(|event| { matches!(event, RunEvent::ToolCallDeclined { .. }) })
        );

        assert_canonical_tool_statuses(
            &origin.store,
            origin.session_id,
            &[ToolLifecycleStatus::Declined],
        );
        assert_canonical_tool_statuses(
            &observer.store,
            observer.session_id,
            &[ToolLifecycleStatus::Cancelled],
        );
    }

    #[tokio::test]
    async fn late_ok_tool_result_uses_typed_terminal_and_preserves_change_evidence() {
        for (behavior, expected_status, expected_file) in [
            (
                TerminalRaceToolBehavior::LateInterruptedResult,
                ToolLifecycleStatus::Cancelled,
                "late-interrupted.txt",
            ),
            (
                TerminalRaceToolBehavior::LateFailedResult,
                ToolLifecycleStatus::Failed,
                "late-failed.txt",
            ),
        ] {
            let mut config = ResolvedConfig::default();
            config.permissions.access_mode = AccessMode::FullAccess;
            let run = run_scripted_with_control_and_tool(
                config,
                vec![scripted_write_call("late_result")],
                RunControl::new(),
                Arc::new(TerminalRaceTool {
                    behavior,
                    start_barrier: None,
                }),
            )
            .await
            .expect("late result run setup");

            match behavior {
                TerminalRaceToolBehavior::LateInterruptedResult => {
                    assert_eq!(
                        run.summary.as_ref().expect("interrupted summary").status,
                        SessionStatus::Cancelled
                    );
                    assert!(canonical_runtime_has_tool_status(
                        &run.store,
                        run.session_id,
                        ToolLifecycleStatus::Cancelled,
                    ));
                }
                TerminalRaceToolBehavior::LateFailedResult => {
                    let summary = run.summary.as_ref().expect("typed failure summary");
                    assert_eq!(summary.status, SessionStatus::Failed);
                    assert_eq!(summary.finish_reason, Some(FinishReason::Error));
                    assert_eq!(summary.interruption_cause, None);
                    assert!(
                        run.events
                            .iter()
                            .any(|event| { matches!(event, RunEvent::ToolCallFailed { .. }) })
                    );
                    assert!(
                        run.events
                            .iter()
                            .any(|event| has_terminal_status(event, TurnTerminalStatus::Failed))
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                !run.events
                    .iter()
                    .any(|event| { matches!(event, RunEvent::ToolCallCompleted { .. }) })
            );
            assert!(run.root.join(expected_file).exists());

            assert_canonical_tool_statuses(&run.store, run.session_id, &[expected_status]);
            let canonical_change_ids = canonical_file_change_ids(&run.store, run.session_id);
            let canonical_history = run
                .store
                .protocol_event_store()
                .list_history_items_for_session(run.session_id)
                .expect("canonical history");
            assert_eq!(
                canonical_change_ids.len(),
                1,
                "expected file={expected_file}; canonical history={canonical_history:#?}"
            );
        }
    }

    #[tokio::test]
    async fn auto_review_routes_detected_risk_through_independent_model_request() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::AutoReview;
        let run = run_scripted(
            config,
            vec![
                ScriptedResponse {
                    events: vec![
                        LlmEvent::ToolCallStart {
                            call_id: "protected_write".to_string(),
                            tool_name: "write".to_string(),
                        },
                        LlmEvent::ToolCallArgsDelta {
                            call_id: "protected_write".to_string(),
                            delta: r#"{"path":"AGENTS.md","content":"temporary test instruction\n"}"#
                                .to_string(),
                        },
                    ],
                    finish_reason: FinishReason::ToolCall,
                },
                ScriptedResponse {
                    events: vec![LlmEvent::TextDelta(
                        r#"{"risk_level":"medium","user_authorization":"high","outcome":"allow","rationale":"The requested scoped workspace write is relevant."}"#
                            .to_string(),
                    )],
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
        assert_eq!(summary.metrics.model_request_count, 3);
        assert_eq!(run.requests.len(), 3);
        assert!(!run.requests[0].tools.is_empty());
        assert!(run.requests[1].tools.is_empty());
        assert!(
            run.requests[1]
                .system_prompt
                .contains("independent permission reviewer")
        );
        let ModelMessage::User { content } = &run.requests[1].messages[0] else {
            panic!("expected reviewer context message");
        };
        assert!(content.contains("write hello.txt"));
        assert!(content.contains("protected_workspace_authority"));
        assert!(!run.requests[2].tools.is_empty());
        assert!(run.confirmations.is_empty());
        assert_eq!(
            std::fs::read_to_string(run.root.join("AGENTS.md"))
                .expect("reviewed write")
                .replace("\r\n", "\n"),
            "temporary test instruction\n"
        );
    }

    #[tokio::test]
    async fn auto_review_denial_falls_back_to_human_confirmation_with_reason() {
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::AutoReview;
        let run = run_scripted(
            config,
            vec![
                ScriptedResponse {
                    events: vec![
                        LlmEvent::ToolCallStart {
                            call_id: "protected_write".to_string(),
                            tool_name: "write".to_string(),
                        },
                        LlmEvent::ToolCallArgsDelta {
                            call_id: "protected_write".to_string(),
                            delta: r#"{"path":"AGENTS.md","content":"human-approved\n"}"#.to_string(),
                        },
                    ],
                    finish_reason: FinishReason::ToolCall,
                },
                ScriptedResponse {
                    events: vec![LlmEvent::TextDelta(
                        r#"{"risk_level":"high","user_authorization":"low","outcome":"deny","rationale":"The authority-file change needs explicit confirmation."}"#
                            .to_string(),
                    )],
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

        assert_eq!(
            run.summary.expect("summary").status,
            SessionStatus::Completed
        );
        assert_eq!(run.confirmations.len(), 1);
        assert!(run.confirmations[0].details.iter().any(|detail| {
            detail.contains("AI reviewer denied this request (risk: high)")
                && detail.contains("authority-file change")
        }));
        assert_eq!(
            std::fs::read_to_string(run.root.join("AGENTS.md"))
                .expect("human-approved write")
                .replace("\r\n", "\n"),
            "human-approved\n"
        );
    }

    #[tokio::test]
    async fn unclassified_provider_cancel_terminalizes_as_failure() {
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
        let error = run
            .summary
            .expect_err("an unclassified provider cancellation must not impersonate user stop");
        let session = run
            .store
            .session_repo()
            .get_session(run.session_id)
            .await
            .expect("session");

        assert!(error.to_string().contains("without a classified cause"));
        assert_eq!(session.status, SessionStatus::Failed);
        assert!(
            run.events
                .iter()
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Failed))
        );
        assert!(
            !run.events
                .iter()
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Interrupted))
        );
    }

    #[tokio::test]
    async fn provider_error_and_length_finish_reasons_fail_even_with_text() {
        for finish_reason in [FinishReason::Error, FinishReason::Length] {
            let run = run_scripted(
                ResolvedConfig::default(),
                vec![ScriptedResponse {
                    events: vec![LlmEvent::TextDelta("partial provider output".to_string())],
                    finish_reason,
                }],
            )
            .await
            .expect("run fixture");
            let error = run
                .summary
                .expect_err("non-success finish reason must fail the turn");
            assert!(matches!(
                (finish_reason, &error),
                (FinishReason::Error, AgentError::ProviderFinishError)
                    | (FinishReason::Length, AgentError::ProviderOutputLimit)
            ));
            assert!(matches!(
                run.run_control.cause(),
                Some(RunCancellationCause::Failure(message))
                    if message == error.to_string()
            ));
            assert_eq!(
                run.store
                    .session_repo()
                    .get_session(run.session_id)
                    .await
                    .expect("session")
                    .status,
                SessionStatus::Failed
            );
            assert!(
                run.events
                    .iter()
                    .any(|event| has_terminal_status(event, TurnTerminalStatus::Failed))
            );
            let terminal = run
                .store
                .protocol_event_store()
                .list_runtime_events_for_session(run.session_id)
                .expect("runtime events")
                .into_iter()
                .rev()
                .find_map(|event| match event.msg {
                    crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } => Some(*terminal),
                    _ => None,
                })
                .expect("canonical failure terminal");
            assert_eq!(
                terminal.final_response_id, None,
                "an uncommitted provider response must not become durable response lineage"
            );
            assert!(
                !run.events
                    .iter()
                    .any(|event| has_terminal_status(event, TurnTerminalStatus::Completed))
            );
        }
    }

    #[tokio::test]
    async fn provider_finish_reason_and_tool_payload_must_agree() {
        let cases = [
            ScriptedResponse {
                events: vec![LlmEvent::TextDelta("text without a tool call".to_string())],
                finish_reason: FinishReason::ToolCall,
            },
            ScriptedResponse {
                events: vec![
                    LlmEvent::ToolCallStart {
                        call_id: "unexpected_tool".to_string(),
                        tool_name: "write".to_string(),
                    },
                    LlmEvent::ToolCallArgsDelta {
                        call_id: "unexpected_tool".to_string(),
                        delta: r#"{"path":"must-not-exist.txt","content":"no"}"#.to_string(),
                    },
                ],
                finish_reason: FinishReason::Stop,
            },
        ];

        for response in cases {
            let run = run_scripted(ResolvedConfig::default(), vec![response])
                .await
                .expect("run fixture");
            assert!(matches!(
                run.summary,
                Err(AgentError::ProviderFinishShape { .. })
            ));
            assert_eq!(
                run.store
                    .session_repo()
                    .get_session(run.session_id)
                    .await
                    .expect("session")
                    .status,
                SessionStatus::Failed
            );
            assert!(!run.root.join("must-not-exist.txt").exists());
        }
    }

    #[tokio::test]
    async fn pending_steer_is_drained_before_the_next_provider_request() {
        let config = ResolvedConfig::default();
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        let steer_text = "also verify the result";
        steer_tx
            .send(ActiveSteerInput {
                history_item_id: crate::protocol::HistoryItemId::new(),
                steer: crate::protocol::SteerTurn {
                    expected_turn_id: TurnId::new(),
                    items: vec![UserInputItem::Text {
                        text: steer_text.to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("steer-test".to_string()),
                },
            })
            .expect("queue steer");
        let run = run_scripted_with_options(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::TextDelta("done".to_string())],
                finish_reason: FinishReason::Stop,
            }],
            None,
            Some(steer_rx),
            None,
        )
        .await
        .expect("run");
        run.summary.expect("summary");

        assert!(run.requests[0].messages.iter().any(
            |message| matches!(message, ModelMessage::User { content } if content == steer_text)
        ));
    }

    #[tokio::test]
    async fn context_overflow_fails_explicitly_without_compaction_or_history_loss() {
        let mut config = ResolvedConfig::default();
        config.session.overflow_margin_tokens = 200_000;
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::TextDelta("must not be requested".to_string())],
                finish_reason: FinishReason::Stop,
            }],
        )
        .await
        .expect("run setup");
        let error = run.summary.expect_err("overflow must fail");
        let error_text = error.to_string();
        let history = run
            .store
            .protocol_event_store()
            .list_history_items_for_session(run.session_id)
            .expect("history");

        assert!(
            error_text.contains("history was left unchanged"),
            "unexpected overflow error: {error_text}"
        );
        assert!(run.requests.is_empty());
        assert!(
            !run.events
                .iter()
                .any(|event| matches!(event, RunEvent::CompactionCompleted { .. }))
        );
        assert!(
            history
                .iter()
                .any(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
        );
        assert!(
            !history
                .iter()
                .any(|item| matches!(item.payload, HistoryItemPayload::Compaction { .. }))
        );
    }

    #[tokio::test]
    async fn metrics_record_the_live_effective_access_mode() {
        let config = ResolvedConfig::default();
        let live = LiveConfigOverrides::new(AccessMode::AutoReview);
        let run = run_scripted_with_options(
            config,
            vec![ScriptedResponse {
                events: vec![LlmEvent::TextDelta("done".to_string())],
                finish_reason: FinishReason::Stop,
            }],
            None,
            None,
            Some(live),
        )
        .await
        .expect("run");
        let summary = run.summary.expect("summary");

        assert_eq!(
            summary.metrics.config.expect("config").access_mode,
            "auto_review"
        );
    }

    #[test]
    fn image_diagnostics_count_decoded_bytes() {
        let message = ModelMessage::UserParts {
            parts: vec![ModelContentPart::Image {
                mime_type: "image/png".to_string(),
                data_base64: "aGVsbG8=".to_string(),
            }],
        };

        assert_eq!(message_image_bytes(&message), 5);
    }

    #[tokio::test]
    async fn loop_failure_terminalizes_session_with_canonical_terminal() {
        let config = ResolvedConfig::default();
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
        let terminal = run
            .store
            .protocol_event_store()
            .list_runtime_events_for_session(run.session_id)
            .expect("runtime events")
            .into_iter()
            .rev()
            .find_map(|event| match event.msg {
                crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } => Some(*terminal),
                _ => None,
            })
            .expect("canonical terminal");

        assert_eq!(session.status, SessionStatus::Failed);
        assert_eq!(terminal.status, TurnTerminalStatus::Failed);
        assert_eq!(terminal.finish_reason, Some(FinishReason::Error));
        assert!(
            run.events
                .iter()
                .any(|event| has_terminal_status(event, TurnTerminalStatus::Failed))
        );
    }

    #[tokio::test]
    async fn active_goal_is_blocked_after_turn_error() {
        let config = ResolvedConfig::default();
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
                    | HistoryItemPayload::AssistantMessage { content, .. } => {
                        Some(content_text(content))
                    }
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
                    finish_reason: FinishReason::ToolCall,
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
    async fn text_and_reasoning_summary_deltas_stream_before_single_text_persist() {
        let config = ResolvedConfig::default();
        let run = run_scripted(
            config,
            vec![ScriptedResponse {
                events: vec![
                    LlmEvent::ReasoningSummaryDelta("work inspected".to_string()),
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
        let reasoning_summary_deltas = run
            .events
            .iter()
            .filter_map(|event| match event {
                RunEvent::ReasoningSummaryDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(reasoning_summary_deltas, vec!["work inspected"]);
        assert_eq!(text_deltas, vec!["hello", " world"]);

        let persisted_assistant_text = run
            .store
            .protocol_event_store()
            .list_history_items_for_session(run.session_id)
            .expect("history")
            .into_iter()
            .filter_map(|item| match item.payload {
                HistoryItemPayload::AssistantMessage { content, .. } => {
                    Some(content_text(&content))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(persisted_assistant_text, vec!["hello world"]);
    }

    #[test]
    fn prompt_asset_stays_small() {
        assert!(include_str!("../../assets/prompts/system.md").len() < 8 * 1024);
        assert!(include_str!("../../assets/prompts/profile_default.md").len() < 2 * 1024);
        assert!(include_str!("../../assets/prompts/collaboration_plan.md").len() < 2 * 1024);
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
                    content: vec![ContentPart::Text {
                        text: "hello".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
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
                    response_id: ModelResponseId::new(),
                    model_call_id: "call_read".to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: serde_json::json!({"path":"README.md"}).to_string(),
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
    fn history_projection_preserves_declined_and_cancelled_call_output_pairs() {
        for status in [
            crate::protocol::ToolLifecycleStatus::Declined,
            crate::protocol::ToolLifecycleStatus::Cancelled,
        ] {
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
                        content: vec![ContentPart::Text {
                            text: "previous request".to_string(),
                        }],
                        prompt_dispatch: None,
                        editor_context: None,
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
                        response_id: ModelResponseId::new(),
                        model_call_id: "call_write".to_string(),
                        tool_name: "write".to_string(),
                        arguments_json: serde_json::json!({
                            "path":"blocked.txt",
                            "content":"no"
                        })
                        .to_string(),
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
                        status,
                        title: "not executed".to_string(),
                        output_text: "approval was not granted".to_string(),
                        metadata: Value::Null,
                        success: None,
                    },
                },
            ];

            let messages = messages_from_history(&items);
            assert_eq!(messages.len(), 3, "status={status:?}");
            assert!(matches!(messages[0], ModelMessage::User { .. }));
            assert!(matches!(
                messages[1],
                ModelMessage::AssistantToolCalls { .. }
            ));
            assert!(matches!(messages[2], ModelMessage::Tool { .. }));
        }
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
                payload: HistoryItemPayload::AssistantMessage {
                    response_id: ModelResponseId::new(),
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
                    content: vec![ContentPart::Text {
                        text: "second turn user".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
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
                    content: vec![ContentPart::Text {
                        text: "old detail".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            },
            HistoryItem {
                id: recent_id,
                session_id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 200,
                payload: HistoryItemPayload::UserTurn {
                    content: vec![ContentPart::Text {
                        text: "recent detail".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            },
            HistoryItem {
                id: compaction_id,
                session_id,
                turn_id,
                sequence_no: 2,
                created_at_ms: 300,
                payload: HistoryItemPayload::Compaction {
                    mode: crate::protocol::CompactionMode::Automatic,
                    summary: "old detail summary".to_string(),
                    replacement_item_ids: vec![old_id],
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
    fn invalid_tool_arguments_remain_raw_while_execution_uses_a_transient_error() {
        let raw = ModelToolCall {
            call_id: "provider-call".to_string(),
            tool_name: "unknown_provider_tool".to_string(),
            arguments_json: "{not-json}".to_string(),
        };

        let prepared = prepare_model_tool_call(ToolCallId::new(), raw.clone(), &[]);

        assert_eq!(prepared.call.call_id, raw.call_id);
        assert_eq!(prepared.call.tool_name, raw.tool_name);
        assert_eq!(prepared.call.arguments_json, raw.arguments_json);
        assert_eq!(prepared.tool, crate::tool::ToolName::Invalid);
        assert_eq!(prepared.arguments, Value::Null);
        assert!(prepared.validation_error.is_some());
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

    struct ScriptedRun {
        summary: Result<RunSummary, AgentError>,
        run_control: RunControl,
        store: StoreBundle,
        session_id: crate::session::SessionId,
        events: Vec<RunEvent>,
        requests: Vec<ChatRequest>,
        confirmations: Vec<crate::tool::PermissionRequest>,
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
        run_scripted_with_options(config, responses, goal, None, None).await
    }

    async fn run_scripted_with_options(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
        goal: Option<(&str, ThreadGoalStatus, Option<i64>)>,
        steer_rx: Option<UnboundedReceiver<ActiveSteerInput>>,
        live_config: Option<LiveConfigOverrides>,
    ) -> Result<ScriptedRun, AgentError> {
        run_scripted_with_options_and_decision(
            config,
            responses,
            goal,
            steer_rx,
            live_config,
            crate::cli::ReviewDecision::Approved,
        )
        .await
    }

    async fn run_scripted_with_options_and_decision(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
        goal: Option<(&str, ThreadGoalStatus, Option<i64>)>,
        steer_rx: Option<UnboundedReceiver<ActiveSteerInput>>,
        live_config: Option<LiveConfigOverrides>,
        review_decision: crate::cli::ReviewDecision,
    ) -> Result<ScriptedRun, AgentError> {
        run_scripted_internal(
            config,
            responses,
            goal,
            steer_rx,
            live_config,
            review_decision,
            RunControl::new(),
            None,
        )
        .await
    }

    async fn run_scripted_with_control_and_tool(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
        run_control: RunControl,
        replacement_tool: Arc<dyn crate::tool::registry::Tool>,
    ) -> Result<ScriptedRun, AgentError> {
        run_scripted_internal(
            config,
            responses,
            None,
            None,
            None,
            crate::cli::ReviewDecision::Approved,
            run_control,
            Some(replacement_tool),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_scripted_internal(
        config: ResolvedConfig,
        responses: Vec<ScriptedResponse>,
        goal: Option<(&str, ThreadGoalStatus, Option<i64>)>,
        steer_rx: Option<UnboundedReceiver<ActiveSteerInput>>,
        live_config: Option<LiveConfigOverrides>,
        review_decision: crate::cli::ReviewDecision,
        run_control: RunControl,
        replacement_tool: Option<Arc<dyn crate::tool::registry::Tool>>,
    ) -> Result<ScriptedRun, AgentError> {
        let run_control_observer = run_control.clone();
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
        let admission_id = store
            .session_repo()
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admit scripted run")
            .expect("scripted run admission");
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "write hello.txt".to_string(),
            }],
            prompt_dispatch: Some(PromptDispatchPart::raw("write hello.txt")),
            editor_context: None,
        };
        session_service
            .store_user_turn_with_protocol_bundle(&session, &admission_id, &user_turn, turn_id, 0)
            .await
            .expect("user turn");
        let context = context_manager::ContextManager::rehydrate(
            store
                .protocol_event_store()
                .list_history_items_for_session(session.session.id)
                .expect("history"),
        );
        let (steer_sender_guard, steer_rx) = if let Some(mut queued) = steer_rx {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            while let Ok(mut input) = queued.try_recv() {
                input.steer.expected_turn_id = turn_id;
                input.history_item_id = store
                    .session_repo()
                    .accept_active_turn_steer(session_id, &input.steer)
                    .await
                    .expect("persist queued steer");
                sender.send(input).expect("forward queued steer");
            }
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let tool_services = test_tool_services(&config, &store, storage_paths);
        let mut registry = ToolRegistry::builtin(tool_services.clone());
        if let Some(tool) = replacement_tool {
            registry.replace_tool_for_test(tool);
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        let llm = Arc::new(ScriptedClient {
            responses: Mutex::new(responses),
            requests: Arc::clone(&requests),
        });
        let agent = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services);
        let next_protocol_sequence_no = store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .expect("protocol position")
            .filter(|(active_turn_id, _)| *active_turn_id == turn_id)
            .map(|(_, sequence_no)| sequence_no)
            .unwrap_or(1);
        let mut sink = CapturingSink {
            events: Vec::new(),
            sequence_no: next_protocol_sequence_no,
        };
        let mut prompt = DecisionPrompt {
            decision: review_decision,
            ..DecisionPrompt::default()
        };
        let summary = agent
            .run(
                AgentRunRequest {
                    session,
                    turn: Arc::new(turn_context::TurnContext {
                        turn_id,
                        admission_id,
                        mode: mode::CollaborationMode::resolve(mode::ModeKind::Default),
                        policy: Arc::new(
                            crate::llm::model_policy::ResolvedTurnPolicy::resolve(
                                &mode::CollaborationMode::resolve(mode::ModeKind::Default),
                                crate::llm::model_policy::ModelPolicy::from_config(&config),
                                crate::llm::model_policy::ProviderCapabilities::from_config(
                                    &config,
                                ),
                                config.model.reasoning_summary,
                            )
                            .expect("turn policy"),
                        ),
                        multi_agent_mode: config
                            .multi_agent
                            .enabled
                            .then_some(config.multi_agent.mode),
                        current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
                    }),
                    context,
                    config: config.clone(),
                    run_control,
                    live_config,
                    steer_rx,
                    agent_context: None,
                },
                &mut prompt,
                &mut sink,
            )
            .await;
        drop(steer_sender_guard);

        Ok(ScriptedRun {
            summary,
            run_control: run_control_observer,
            store,
            session_id,
            events: sink.events,
            requests: requests.lock().expect("requests mutex").clone(),
            confirmations: prompt.requests,
            root,
        })
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
}

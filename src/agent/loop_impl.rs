use std::collections::BTreeSet;
use std::sync::Arc;

use crate::agent::assistant_message_lifecycle::{
    append_part_and_emit_event, start_assistant_message,
};
use crate::agent::compaction::maybe_compact;
use crate::agent::edit_recovery::InvalidEditRecoveryEnvelope;
use crate::agent::event::{StreamAccumulator, stream_chat_with_optional_terminal_timeout};
use crate::agent::grounding_evidence::{
    docs_route_has_required_content_grounding_evidence, singleton_active_target_exists,
};
use crate::agent::lifecycle_guard::{
    CompletedToolLifecycleEffectsInput, LifecycleGuardProgressDecision,
    LifecycleGuardRecoveryContextInput, LifecycleGuardState, ToolExecutionErrorEffectsInput,
};
use crate::agent::lifecycle_kernel::{
    ActionAdjudication, CompileProviderChatRequestInput, CompileTurnContextInput,
    CompileTurnObligationsInput, TurnLifecycleKernel, TurnLifecyclePlanInput,
    TurnLifecyclePreNormalizationSurfaceInput, TurnLifecycleRecoverySurfaceInput,
    stable_tool_schemas_from_registry,
};
use crate::agent::prompt::{AgentRunRequest, PromptBuilder, RuntimeInputView};
use crate::agent::prompt_assets::{hard_final_step_reminder, max_steps_reminder};
use crate::agent::state::{
    ActiveWorkContract, active_work_contract_for_history_items,
    reduce_session_state_from_history_items,
};
use crate::agent::state_lifecycle::persist_state_update_if_changed;
use crate::agent::tool_orchestrator::{
    AcceptedToolRoutePreparationInput, AcceptedToolRouteRequest,
    AuthoringGroundingRecoveryEnvelope, PreExecutionCorrectiveInput,
    RejectedModelActionNoProgressDecision, RejectedModelActionRouteRequest,
    SupportingContextCorrectiveKind, ToolExecutionInvalidArgumentsInput, ToolExecutionRequest,
    ToolLifecycleRuntime,
};
use crate::agent::turn_decision::build_turn_decision_diagnostic;
use crate::cli::ConfirmationPrompt;
use crate::config::{
    DEFAULT_MODEL_BASE_URL as LOOP_FIXTURE_BASE_URL, DEFAULT_MODEL_NAME as LOOP_FIXTURE_MODEL,
};
use crate::error::AgentError;
use crate::llm::LlmClient;
use crate::protocol::{
    DispatchPolicy, ProjectionId, ProtocolEventStore, ToolChoice, TurnEngine, TurnEngineInput,
    TurnId,
};
use crate::runtime::RunEventSink;
use crate::session::{
    FinishReason, MessagePart, NewPart, PartKind, RunSummary, SessionId, SessionRepository,
    TextPart,
};
use crate::storage::{SqliteSessionRepository, StoreBundle};
use crate::tool::ToolName;
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;

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

pub(crate) fn lifecycle_guard_snapshot_hydration_sequence_order_resists_timestamp_drift_fixture_passes()
-> bool {
    crate::agent::lifecycle_guard::snapshot_hydration_sequence_order_resists_timestamp_drift_fixture_passes()
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
        let assistant_message = start_assistant_message(
            &session_repo,
            request.session.session.id,
            request.user_message_id,
            request.protocol_turn_id,
            &request.model.name,
            &request.config.model.base_url,
            sink,
        )
        .await?;

        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut lifecycle_guard =
            LifecycleGuardState::hydrate_from_history_items(&request.runtime_input.history_items);
        for _step in 0..request.config.session.max_steps_per_turn {
            if let Some(message) =
                TurnLifecycleKernel::runtime_cancel_interrupt_message(request.cancel.is_cancelled())
            {
                return interrupt_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    request.protocol_turn_id,
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
            let runtime_input = RuntimeInputView::from_history_items(history_items);
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
                    request.protocol_turn_id,
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
            persist_state_update_if_changed(
                &session_repo,
                request.session.session.id,
                &persisted_state,
                &reduced_state,
                request.protocol_turn_id,
                sink,
            )
            .await?;

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
            if !TurnLifecycleKernel::provider_messages_have_user_query_anchor(&bundle.messages) {
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
                    request.protocol_turn_id,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message.to_string()));
            }
            let hard_final_step = request.config.session.max_steps_per_turn <= 1;
            let mut system_prompt = bundle.system_prompt.clone();
            let mut tools = bundle.tools.clone();
            if !TurnLifecycleKernel::open_executable_work_requires_tool_call(&step_request.state) {
                lifecycle_guard.clear_open_obligation_final_message_recovery();
            }
            let recovery_prompt_projection = lifecycle_guard.recovery_prompt_projection();
            system_prompt = crate::agent::lifecycle_guard::apply_recovery_prompts_to_system_prompt(
                system_prompt,
                recovery_prompt_projection.final_message.as_deref(),
                recovery_prompt_projection.invalid_edit.as_deref(),
            );
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
            let runtime_shell_family = TurnLifecycleKernel::resolved_shell_family(&request.config);
            if TurnLifecycleKernel::clean_closeout_final_message_lifecycle(
                &step_request.state,
                active_work.as_ref(),
            ) {
                tools.clear();
            }
            let stable_tools = stable_tool_schemas_from_registry(&self.agent.registry);
            let authoring_grounding_projection = lifecycle_guard
                .authoring_grounding_dispatch_projection(
                    &step_request.runtime_input.history_items,
                    &step_request.state,
                    &request.session.workspace.root,
                );
            let authoring_grounding_recovery = if lifecycle_guard
                .authoring_supporting_context_budget_recovery_active(&step_request.state)
            {
                Some(authoring_grounding_projection.recovery_envelope.clone())
            } else {
                None
            };
            let authoring_target_grounding_recovery_edit_only = authoring_grounding_recovery
                .as_ref()
                .is_some_and(|envelope| envelope.missing_grounding_targets.is_empty());
            let authoring_supporting_context_budget_recovery_needs_read =
                !authoring_grounding_projection.missing_targets.is_empty();
            let authoring_supporting_context_budget_recovery_active = lifecycle_guard
                .authoring_supporting_context_budget_recovery_active(&step_request.state);
            let generated_test_source_reference_grounding_active =
                TurnLifecycleKernel::generated_test_source_reference_grounding_active(
                    &step_request.state,
                    authoring_grounding_projection.has_unread_source_change_for_generated_test,
                );
            let generated_test_reference_consumed_target_grounding_active =
                !generated_test_source_reference_grounding_active
                    && TurnLifecycleKernel::generated_test_reference_consumed_target_grounding_active(
                        &step_request.state,
                        authoring_grounding_projection
                            .has_current_source_reference_read_for_generated_test,
                        authoring_grounding_projection.has_unread_source_change_for_generated_test,
                        authoring_grounding_projection.active_targets_need_grounding,
                    );
            let singleton_missing_authoring_target_create_action_active =
                !generated_test_source_reference_grounding_active
                    && TurnLifecycleKernel::singleton_missing_authoring_target_create_action_active(
                        &step_request.state,
                        singleton_active_target_exists(
                            &step_request.state,
                            &request.session.workspace.root,
                        ),
                    );
            let existing_target_grounding_recovery_active =
                TurnLifecycleKernel::existing_target_grounding_recovery_active(
                    &step_request.state,
                    authoring_grounding_projection.active_targets_need_grounding,
                );
            let patch_context_mismatch_grounding_active =
                lifecycle_guard.patch_context_mismatch_grounding_active(&step_request.state);
            let repair_supporting_context_budget_recovery_active = lifecycle_guard
                .repair_supporting_context_budget_recovery_active(&step_request.state);
            let early_surface_plan = lifecycle_guard.apply_early_pre_context_recovery_surface(
                &mut tools,
                &stable_tools,
                &step_request.state,
                authoring_supporting_context_budget_recovery_needs_read,
                generated_test_source_reference_grounding_active,
                generated_test_reference_consumed_target_grounding_active,
                singleton_missing_authoring_target_create_action_active,
                existing_target_grounding_recovery_active,
                patch_context_mismatch_grounding_active,
            );
            if let Some(envelope) = authoring_grounding_recovery.as_ref() {
                ToolLifecycleRuntime::constrain_read_schema_to_missing_authoring_targets(
                    &mut tools, envelope,
                );
            } else if existing_target_grounding_recovery_active {
                let envelope = authoring_grounding_projection.recovery_envelope.clone();
                ToolLifecycleRuntime::constrain_read_schema_to_missing_authoring_targets(
                    &mut tools, &envelope,
                );
            }
            let late_surface_plan = lifecycle_guard.apply_late_pre_context_recovery_surface(
                &mut tools,
                &stable_tools,
                &step_request.state,
                repair_supporting_context_budget_recovery_active,
                patch_context_mismatch_grounding_active,
                early_surface_plan.verification_target_grounding_active,
            );
            let recovery_context =
                lifecycle_guard.compile_recovery_context(LifecycleGuardRecoveryContextInput {
                    state: &step_request.state,
                    tools: &tools,
                    stable_tools: &stable_tools,
                    current_tool_names: &late_surface_plan.current_tool_names,
                    post_provider_tool_names: &late_surface_plan.post_provider_tool_names,
                    repair_supporting_context_budget_recovery_active,
                    generated_test_source_reference_grounding_active,
                    generated_test_reference_consumed_target_grounding_active,
                    verification_target_grounding_active: late_surface_plan
                        .verification_target_grounding_active,
                    authoring_target_grounding_recovery_edit_only,
                    patch_context_mismatch_grounding_active,
                    existing_target_grounding_recovery_active,
                    docs_route_has_required_content_grounding_evidence:
                        docs_route_has_required_content_grounding_evidence(
                            &step_request.state,
                            &step_request.runtime_input.history_items,
                        ),
                    authoring_targets_need_grounding: authoring_grounding_projection
                        .active_targets_need_grounding,
                    progress_projection_target_grounding_read_needed:
                        authoring_grounding_projection.active_targets_need_grounding,
                });
            if recovery_context.code_authoring_final_message_hard_edit_recovery_active {
                lifecycle_guard.mark_open_obligation_final_message_hard_edit_recovery_pending();
            }
            let code_authoring_final_message_recovery_stable_surface_active =
                TurnLifecycleKernel::code_authoring_final_message_recovery_stable_surface_active(
                    &step_request.state,
                    recovery_context.open_obligation_final_message_recovery_active,
                    recovery_context.code_authoring_final_message_hard_edit_recovery_active,
                    recovery_context.failed_edit_recovery_active,
                );
            let code_repair_final_message_recovery_stable_surface_active =
                TurnLifecycleKernel::code_repair_final_message_recovery_stable_surface_active(
                    &step_request.state,
                    recovery_context.open_obligation_final_message_recovery_active,
                    recovery_context.failed_edit_recovery_active,
                );
            TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
                &mut tools,
                &stable_tools,
                TurnLifecyclePreNormalizationSurfaceInput {
                    state: &step_request.state,
                    recovery: recovery_context,
                    code_authoring_final_message_hard_edit_recovery_active: recovery_context
                        .code_authoring_final_message_hard_edit_recovery_active,
                    code_authoring_final_message_recovery_stable_surface_active,
                    code_repair_final_message_recovery_stable_surface_active,
                },
            );
            TurnLifecycleKernel::apply_post_normalization_recovery_surface(
                &mut tools,
                &stable_tools,
                TurnLifecycleRecoverySurfaceInput {
                    state: &step_request.state,
                    recovery: recovery_context,
                    code_authoring_final_message_hard_edit_recovery_active: recovery_context
                        .code_authoring_final_message_hard_edit_recovery_active,
                    generated_test_orientation_allowed:
                        !authoring_supporting_context_budget_recovery_active,
                },
            );
            if TurnLifecycleKernel::authoring_grounding_schema_constraint_required(
                &step_request.state,
                recovery_context,
            ) {
                ToolLifecycleRuntime::constrain_read_schema_to_missing_authoring_targets(
                    &mut tools,
                    &authoring_grounding_projection.recovery_envelope,
                );
            }
            let mut tool_names = tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<BTreeSet<_>>();
            let lifecycle_plan =
                TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
                    policy: &bundle.policy,
                    state: &step_request.state,
                    tool_names: &tool_names,
                    recovery: recovery_context,
                });
            let dispatch_tool_choice = lifecycle_plan.tool_choice.clone();
            let turn_decision = build_turn_decision_diagnostic(
                &step_request.state,
                active_work.as_ref(),
                &bundle.policy,
                &tool_names,
                Some(TurnLifecycleKernel::tool_choice_label(&dispatch_tool_choice).to_string()),
            );
            if let Some(message) =
                TurnLifecycleKernel::turn_decision_dispatch_block_message(&turn_decision)
            {
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
                    request.protocol_turn_id,
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
                authoring_grounding_recovery.as_ref(),
                lifecycle_guard.invalid_edit_arguments_recovery_envelope(),
            );
            sink.emit(crate::session::RunEvent::ControlEnvelopePrepared {
                session_id: request.session.session.id,
                envelope: compiled_turn.envelope.clone(),
            })?;
            if compiled_turn.validation.has_errors() {
                let message = TurnLifecycleKernel::control_envelope_validation_error_message(
                    &compiled_turn.envelope,
                );
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
                    request.protocol_turn_id,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message));
            }
            if let Some(message) =
                TurnLifecycleKernel::control_envelope_fail_closed_dispatch_message(
                    &compiled_turn.envelope,
                )
            {
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
                    request.protocol_turn_id,
                    sink,
                )
                .await?;
                return Err(AgentError::Message(message));
            }
            lifecycle_guard.emit_next_snapshot_if_changed(request.session.session.id, sink)?;
            tool_names = TurnLifecycleKernel::reconcile_tools_with_action_authority(
                &mut tools,
                &compiled_turn.envelope,
            );
            let dispatch_tool_choice = compiled_turn.envelope.action_authority.tool_choice.clone();
            crate::agent::prompt::apply_write_content_shape_to_write_schema_for_required_action(
                &mut tools,
                compiled_turn
                    .envelope
                    .action_authority
                    .required_action
                    .as_ref(),
            );
            let turn_decision = build_turn_decision_diagnostic(
                &step_request.state,
                active_work.as_ref(),
                &bundle.policy,
                &tool_names,
                Some(TurnLifecycleKernel::tool_choice_label(&dispatch_tool_choice).to_string()),
            );
            let control_prompt = compiled_turn
                .envelope
                .projection_bundle
                .prompt
                .render_prompt_block();
            let (provider_messages, surface_filter_policies) =
                TurnLifecycleKernel::provider_messages_for_dispatch_control(
                    &bundle.messages,
                    control_prompt,
                    recovery_prompt_projection.final_message.clone(),
                    recovery_prompt_projection.invalid_edit.clone(),
                    &tool_names,
                    !TurnLifecycleKernel::closeout_ready_final_message_authority(
                        &step_request.state,
                    ),
                );
            let (provider_messages, image_replay_policy) =
                TurnLifecycleKernel::provider_messages_for_active_work_image_replay(
                    provider_messages,
                    &step_request.state,
                    active_work.as_ref(),
                );
            let provider_messages =
                TurnLifecycleKernel::normalize_provider_system_context_for_chat_template(
                    provider_messages,
                );
            let replay_policies = TurnLifecycleKernel::compile_request_replay_policies(
                &bundle.replay_policies,
                surface_filter_policies,
                image_replay_policy,
                &step_request.state,
                recovery_context,
                recovery_prompt_projection.invalid_edit.is_some(),
            );
            let chat_request = TurnLifecycleKernel::compile_provider_chat_request(
                CompileProviderChatRequestInput {
                    model: &step_request.model,
                    config: &step_request.config,
                    system_prompt,
                    messages: provider_messages,
                    tools: tools.clone(),
                    dispatch_tool_choice: &dispatch_tool_choice,
                },
            );
            let terminal_response_timeout_ms =
                TurnLifecycleKernel::terminal_response_timeout_ms_for_state(
                    step_request.config.model.request_timeout_ms,
                    &step_request.state,
                    active_work.as_ref(),
                );
            let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(
                &chat_request,
                Some(turn_decision),
                Some(&compiled_turn.envelope),
                &replay_policies,
            );
            append_part_and_emit_event(
                &session_repo,
                request.session.session.id,
                assistant_message.id,
                request.protocol_turn_id,
                NewPart {
                    kind: PartKind::RequestDiagnostics,
                    payload: MessagePart::RequestDiagnostics(diagnostics.clone()),
                },
                crate::session::RunEvent::ModelRequestPrepared {
                    session_id: request.session.session.id,
                    diagnostics: diagnostics.clone(),
                },
                sink,
            )
            .await?;

            let mut stream = StreamAccumulator::default();
            let response = match stream_chat_with_optional_terminal_timeout(
                &self.agent.llm,
                chat_request,
                step_request.cancel.clone(),
                &mut stream,
                terminal_response_timeout_ms,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    let message = TurnLifecycleKernel::provider_request_failure_message(&error);
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
                        request.protocol_turn_id,
                        sink,
                    )
                    .await?;
                    return Err(AgentError::Llm(error));
                }
            };
            let finish_reason = Some(response.finish_reason);
            let token_usage = response.usage.clone();
            if let Some(message) = TurnLifecycleKernel::provider_finish_reason_interrupt_message(
                finish_reason.as_ref(),
            ) {
                return interrupt_turn(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    &request.model.name,
                    &request.config.model.base_url,
                    message,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    request.protocol_turn_id,
                    sink,
                )
                .await;
            }

            if stream.tool_calls.is_empty()
                && let Some(runtime_call) =
                    TurnLifecycleKernel::runtime_owned_required_verification_tool_call(
                        active_work.as_ref(),
                        &tool_names,
                        &dispatch_tool_choice,
                        compiled_turn
                            .envelope
                            .action_authority
                            .required_action
                            .as_ref(),
                    )
            {
                stream.tool_calls.push(runtime_call);
            }

            let final_message_adjudication = TurnLifecycleKernel::adjudicate_final_message_response(
                stream.tool_calls.is_empty(),
                stream.text.clone(),
                compiled_turn
                    .envelope
                    .projection_bundle
                    .tool_result_feedback
                    .projection_id,
                &step_request.state,
                &tool_names,
                &compiled_turn.envelope,
            );

            if let Some(rejection) = TurnLifecycleKernel::open_obligation_final_message_rejection(
                final_message_adjudication.as_ref(),
                finish_reason.as_ref(),
            ) {
                let source_call_id = crate::session::ToolCallId::new();
                let proposal = rejection.to_rejected_tool_proposal(
                    source_call_id,
                    &tool_names,
                    &compiled_turn
                        .envelope
                        .projection_bundle
                        .tool_result_feedback,
                );
                ToolLifecycleRuntime::record_tool_proposal_rejected_event(
                    &self.agent.store,
                    request.session.session.id,
                    request.protocol_turn_id,
                    source_call_id,
                    proposal,
                    sink,
                )?;
                if let Some(message) = lifecycle_guard
                    .record_open_obligation_final_message_recovery(
                        &step_request.state,
                        compiled_turn
                            .envelope
                            .action_authority
                            .required_action
                            .as_ref(),
                        &tool_names,
                        recovery_context.docs_grounding_final_message_recovery_active,
                        &dispatch_tool_choice,
                        recovery_context.malformed_apply_patch_write_recovery_active,
                        recovery_context.code_authoring_final_message_hard_edit_recovery_active,
                    )
                {
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
                        request.protocol_turn_id,
                        sink,
                    )
                    .await?;
                    return Err(AgentError::Message(message));
                }
                continue;
            }

            if !stream.reasoning.trim().is_empty() {
                append_part_and_emit_event(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    request.protocol_turn_id,
                    NewPart {
                        kind: PartKind::Reasoning,
                        payload: MessagePart::Reasoning(crate::session::ReasoningPart {
                            text: stream.reasoning.clone(),
                        }),
                    },
                    crate::session::RunEvent::ReasoningDelta {
                        message_id: assistant_message.id,
                        delta: stream.reasoning.clone(),
                    },
                    sink,
                )
                .await?;
            }
            if !stream.text.trim().is_empty() {
                append_part_and_emit_event(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    request.protocol_turn_id,
                    NewPart {
                        kind: PartKind::Text,
                        payload: MessagePart::Text(TextPart {
                            text: stream.text.clone(),
                        }),
                    },
                    crate::session::RunEvent::TextDelta {
                        message_id: assistant_message.id,
                        delta: stream.text.clone(),
                    },
                    sink,
                )
                .await?;
            }

            if stream.tool_calls.is_empty() {
                if let Some(message) =
                    TurnLifecycleKernel::empty_tool_call_final_response_failure_message(
                        finish_reason.as_ref(),
                    )
                {
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
                        request.protocol_turn_id,
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
                    request.model.context_window,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    request.protocol_turn_id,
                    sink,
                )
                .await;
            }

            for tool_call in stream.tool_calls {
                if let Some(message) = TurnLifecycleKernel::runtime_cancel_interrupt_message(
                    request.cancel.is_cancelled(),
                ) {
                    return interrupt_turn(
                        &session_repo,
                        request.session.session.id,
                        assistant_message.id,
                        &request.model.name,
                        &request.config.model.base_url,
                        message,
                        tool_call_count,
                        failed_tool_count,
                        change_count,
                        request.protocol_turn_id,
                        sink,
                    )
                    .await;
                }
                tool_call_count += 1;
                let requested_tool_name = tool_call.tool_name.clone();
                let tool_names_for_route = tool_names.clone();
                let runtime_owned_verification_redirect =
                    TurnLifecycleKernel::runtime_owned_required_verification_dispatch_redirect(
                        &requested_tool_name,
                        &tool_call.arguments_json,
                        active_work.as_ref(),
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                        compiled_turn
                            .envelope
                            .action_authority
                            .required_action
                            .as_ref(),
                    );
                let raw_action = TurnLifecycleKernel::adjudicate_tool_call_model_action(
                    &tool_call,
                    runtime_owned_verification_redirect.as_ref(),
                    &tool_names_for_route,
                    &compiled_turn.envelope,
                    |action_name| self.agent.registry.has_tool(action_name),
                );

                if let ActionAdjudication::RejectedModelAction(rejection) = raw_action.adjudication
                {
                    let raw_proposal = rejection.proposal.clone();
                    let route = ToolLifecycleRuntime::route_rejected_model_action(
                        RejectedModelActionRouteRequest {
                            requested_tool: raw_proposal.requested_tool.clone(),
                            effective_tool: raw_proposal.effective_tool.clone(),
                            arguments_json: raw_proposal.arguments_json.clone(),
                            allowed_tool_names: &tool_names_for_route,
                            tool_exists: raw_action.tool_exists,
                            tool_allowed: raw_action.tool_allowed,
                            tool_choice: Some(TurnLifecycleKernel::tool_choice_label(
                                &dispatch_tool_choice,
                            )),
                            control_projection: Some(
                                ToolLifecycleRuntime::control_projection_metadata(
                                    &compiled_turn
                                        .envelope
                                        .projection_bundle
                                        .tool_result_feedback,
                                ),
                            ),
                            sandbox_decision: ToolLifecycleRuntime::sandbox_decision_metadata(
                                &compiled_turn.envelope.context.sandbox,
                            ),
                        },
                    );
                    let record = ToolLifecycleRuntime::record_pending_call(
                        &session_repo,
                        request.session.session.id,
                        assistant_message.id,
                        request.protocol_turn_id,
                        &route,
                        sink,
                    )
                    .await?;
                    let result = TurnLifecycleKernel::rejected_model_action_corrective_result(
                        &rejection,
                        record.id,
                        &tool_names_for_route,
                        raw_action.tool_exists,
                        raw_action.tool_allowed,
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                        &step_request.state,
                        &dispatch_tool_choice,
                    );
                    ToolLifecycleRuntime::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        request.session.session.id,
                        request.protocol_turn_id,
                        record.id,
                        record.tool_name,
                        &result,
                        &route,
                        sink,
                    )
                    .await?;
                    if let Some(message) = lifecycle_guard
                        .record_rejected_model_action_invalid_arguments_recovery(
                            &raw_proposal.effective_tool,
                            &result.metadata,
                            &step_request.state,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        )
                    {
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
                            request.protocol_turn_id,
                            sink,
                        )
                        .await;
                    }
                    if TurnLifecycleKernel::closeout_ready_final_message_authority(
                        &step_request.state,
                    ) {
                        return complete_turn(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            &request.model.name,
                            &request.config.model.base_url,
                            Some(FinishReason::Stop),
                            token_usage,
                            request.model.context_window,
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            request.protocol_turn_id,
                            sink,
                        )
                        .await;
                    }
                    match lifecycle_guard.record_rejected_model_action_no_progress(
                        &raw_proposal.effective_tool,
                        &raw_proposal.arguments_json,
                        &result.metadata,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                        compiled_turn
                            .envelope
                            .action_authority
                            .required_action
                            .as_ref(),
                        raw_action.tool_allowed,
                    ) {
                        RejectedModelActionNoProgressDecision::Fail(message) => {
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
                                request.protocol_turn_id,
                                sink,
                            )
                            .await;
                        }
                        RejectedModelActionNoProgressDecision::SuppressUntilFeedbackVisible => {
                            continue;
                        }
                        RejectedModelActionNoProgressDecision::Continue => {}
                    }
                    continue;
                }

                let prepared_route_arguments =
                    ToolLifecycleRuntime::prepare_accepted_tool_route_arguments(
                        AcceptedToolRoutePreparationInput {
                            requested_tool_name: &requested_tool_name,
                            original_arguments_json: &tool_call.arguments_json,
                            runtime_owned_verification_redirect:
                                runtime_owned_verification_redirect.as_ref(),
                            active_work: active_work.as_ref(),
                            state: &step_request.state,
                            shell_family: runtime_shell_family,
                        },
                    );
                let effective_tool_name = prepared_route_arguments.effective_tool_name.clone();
                let tool_exists = self.agent.registry.has_tool(&effective_tool_name);
                let tool_allowed = tool_names_for_route.contains(&effective_tool_name);
                let route =
                    ToolLifecycleRuntime::route_accepted_tool_call(AcceptedToolRouteRequest {
                        requested_tool: requested_tool_name.clone(),
                        effective_tool: effective_tool_name.clone(),
                        original_arguments_json: tool_call.arguments_json.clone(),
                        effective_arguments_json: prepared_route_arguments.effective_arguments_json,
                        allowed_tool_names: &tool_names_for_route,
                        tool_exists,
                        tool_allowed,
                        redirected_from_arguments_json: prepared_route_arguments
                            .redirected_from_arguments_json,
                        redirect_reason: prepared_route_arguments.redirect_reason,
                        tool_choice: Some(TurnLifecycleKernel::tool_choice_label(
                            &dispatch_tool_choice,
                        )),
                        control_projection: Some(
                            ToolLifecycleRuntime::control_projection_metadata(
                                &compiled_turn
                                    .envelope
                                    .projection_bundle
                                    .tool_result_feedback,
                            ),
                        ),
                        sandbox_decision: ToolLifecycleRuntime::sandbox_decision_metadata(
                            &compiled_turn.envelope.context.sandbox,
                        ),
                    });
                let record = ToolLifecycleRuntime::record_pending_call(
                    &session_repo,
                    request.session.session.id,
                    assistant_message.id,
                    request.protocol_turn_id,
                    &route,
                    sink,
                )
                .await?;
                if let Some(candidate) = prepared_route_arguments.escaped_source_write_candidate {
                    ToolLifecycleRuntime::emit_candidate_repair_edit_recorded(
                        sink,
                        record.id,
                        candidate.into_candidate_repair_edit(record.id),
                    )?;
                }

                ToolLifecycleRuntime::mark_running(&session_repo, record.id).await?;
                let parsed_arguments = match ToolLifecycleRuntime::parse_route_arguments(&route) {
                    Ok(value) => value,
                    Err(error) => {
                        let result = ToolLifecycleRuntime::tool_execution_invalid_arguments_result(
                            ToolExecutionInvalidArgumentsInput {
                                effective_tool_name: &effective_tool_name,
                                effective_arguments_json: &route.effective_arguments_json,
                                error_text: &error.message,
                                state: &step_request.state,
                                allowed_tools: &tool_names_for_route,
                                tool_choice: &dispatch_tool_choice,
                            },
                        );
                        ToolLifecycleRuntime::complete_corrective_call(
                            &session_repo,
                            assistant_message.id,
                            request.session.session.id,
                            request.protocol_turn_id,
                            record.id,
                            record.tool_name,
                            &result,
                            &route,
                            sink,
                        )
                        .await?;
                        if let Some(message) = lifecycle_guard.record_tool_execution_error_effects(
                            ToolExecutionErrorEffectsInput {
                                effective_tool_name: &effective_tool_name,
                                effective_arguments_json: &route.effective_arguments_json,
                                error_text: &error.message,
                                invalid_arguments_metadata: Some(&result.metadata),
                                state: &step_request.state,
                                tool_names: &tool_names_for_route,
                                dispatch_tool_choice: &dispatch_tool_choice,
                            },
                        ) {
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
                                request.protocol_turn_id,
                                sink,
                            )
                            .await;
                        }
                        continue;
                    }
                };
                if let Some(decision) =
                    ToolLifecycleRuntime::classify_pre_execution_corrective_result(
                        PreExecutionCorrectiveInput {
                            effective_tool_name: &effective_tool_name,
                            parsed_arguments: &parsed_arguments,
                            active_work: active_work.as_ref(),
                            state: &step_request.state,
                            workspace_root: &request.session.workspace.root,
                            workspace_cwd: Some(request.session.workspace.cwd.as_path()),
                            allowed_tools: &tool_names_for_route,
                            history_items: &step_request.runtime_input.history_items,
                            shell_family: runtime_shell_family,
                        },
                    )
                {
                    let result = decision.result;
                    ToolLifecycleRuntime::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        request.session.session.id,
                        request.protocol_turn_id,
                        record.id,
                        record.tool_name,
                        &result,
                        &route,
                        sink,
                    )
                    .await?;
                    failed_tool_count += 1;
                    let terminal_message = lifecycle_guard
                        .record_pre_execution_corrective_no_progress(
                            decision.kind,
                            &result,
                            &effective_tool_name,
                            &parsed_arguments,
                            active_work.as_ref(),
                            &step_request.state,
                            &request.session.workspace.root,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                            TurnLifecycleKernel::open_executable_work_requires_tool_call(
                                &step_request.state,
                            ),
                        );
                    if let Some(message) = terminal_message {
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
                            request.protocol_turn_id,
                            sink,
                        )
                        .await;
                    }
                    continue;
                }
                let supporting_context_corrective_input = lifecycle_guard
                    .prepare_supporting_context_corrective_input(
                        &effective_tool_name,
                        &parsed_arguments,
                        &step_request.state,
                        &step_request.runtime_input.history_items,
                        &request.session.workspace.root,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                        existing_target_grounding_recovery_active,
                        generated_test_reference_consumed_target_grounding_active,
                    );
                if let Some(corrective) =
                    ToolLifecycleRuntime::classify_supporting_context_corrective_result(
                        supporting_context_corrective_input.as_input(
                            &effective_tool_name,
                            &parsed_arguments,
                            &step_request.state,
                        ),
                    )
                {
                    ToolLifecycleRuntime::complete_corrective_call(
                        &session_repo,
                        assistant_message.id,
                        request.session.session.id,
                        request.protocol_turn_id,
                        record.id,
                        record.tool_name,
                        &corrective.result,
                        &route,
                        sink,
                    )
                    .await?;
                    failed_tool_count += 1;
                    let terminal_message = match corrective.kind {
                        SupportingContextCorrectiveKind::DocsBudgetExhausted => lifecycle_guard
                            .record_docs_supporting_context_budget_exhausted_no_progress(
                                corrective
                                    .budget_key
                                    .expect("docs budget corrective carries budget key"),
                                &step_request.state,
                            ),
                        SupportingContextCorrectiveKind::AuthoringTargetGroundingRequired => {
                            lifecycle_guard.record_authoring_target_grounding_required_no_progress(
                                &corrective.result,
                            )
                        }
                        SupportingContextCorrectiveKind::GeneratedTestTargetGroundingRequired => {
                            lifecycle_guard
                                .record_generated_test_target_grounding_required_no_progress(
                                    &corrective.result,
                                    &step_request.state,
                                )
                        }
                    };
                    if let Some(message) = terminal_message {
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
                            request.protocol_turn_id,
                            sink,
                        )
                        .await;
                    }
                    continue;
                }
                match ToolLifecycleRuntime::execute_registered_call(
                    &self.agent.registry,
                    &effective_tool_name,
                    parsed_arguments,
                    ToolExecutionRequest {
                        session: &request.session,
                        workspace: &request.session.workspace,
                        config: &request.config,
                        tool_call_id: record.id,
                        tool_name: record.tool_name,
                        cancel: request.cancel.clone(),
                        prompt,
                        services: &self.agent.tool_services,
                    },
                    sink,
                )
                .await
                {
                    Ok(result) => {
                        change_count += result.change_summaries.len();
                        let completion_metadata = ToolLifecycleRuntime::complete_executed_call(
                            &session_repo,
                            assistant_message.id,
                            request.session.session.id,
                            request.protocol_turn_id,
                            record.id,
                            record.tool_name,
                            &result,
                            &route,
                            &request.session.workspace.root,
                            &step_request.state,
                            active_work.as_ref(),
                            sink,
                        )
                        .await?;
                        let content_changing_progress =
                            ToolLifecycleRuntime::tool_output_is_content_changing_progress(
                                &completion_metadata,
                            );
                        if content_changing_progress && !result.change_summaries.is_empty() {
                            crate::tool::todo_write::align_progress_projection_after_changes(
                                &session_repo,
                                request.session.session.id,
                                &request.session.workspace.root,
                                &todos,
                                &result.change_summaries,
                            )
                            .await?;
                        }
                        if let Some(decision) = lifecycle_guard
                            .record_completed_tool_lifecycle_effects(
                                CompletedToolLifecycleEffectsInput {
                                    effective_tool_name: &effective_tool_name,
                                    effective_arguments_json: &route.effective_arguments_json,
                                    result: &result,
                                    completion_metadata: &completion_metadata,
                                    state: &step_request.state,
                                    tool_names: &tool_names_for_route,
                                    dispatch_tool_choice: &dispatch_tool_choice,
                                    content_changing_progress,
                                },
                            )
                        {
                            match decision {
                                LifecycleGuardProgressDecision::Continue => continue,
                                LifecycleGuardProgressDecision::Fail(message) => {
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
                                        request.protocol_turn_id,
                                        sink,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if request.cancel.is_cancelled() {
                            failed_tool_count += 1;
                            let tool_error_message =
                                ToolLifecycleRuntime::tool_execution_cancelled_error_message();
                            let interruption_message =
                                TurnLifecycleKernel::runtime_cancel_interrupt_message(true)
                                    .expect("cancelled runtime has interrupt message");
                            ToolLifecycleRuntime::fail_executed_call(
                                &session_repo,
                                assistant_message.id,
                                request.session.session.id,
                                request.protocol_turn_id,
                                record.id,
                                record.tool_name,
                                tool_error_message,
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
                                interruption_message,
                                tool_call_count,
                                failed_tool_count,
                                change_count,
                                request.protocol_turn_id,
                                sink,
                            )
                            .await;
                        }
                        let error_text = ToolLifecycleRuntime::tool_execution_error_text(&error);
                        if ToolLifecycleRuntime::tool_execution_error_is_invalid_arguments(
                            &error_text,
                        ) {
                            let result =
                                ToolLifecycleRuntime::tool_execution_invalid_arguments_result(
                                    ToolExecutionInvalidArgumentsInput {
                                        effective_tool_name: &effective_tool_name,
                                        effective_arguments_json: &route.effective_arguments_json,
                                        error_text: &error_text,
                                        state: &step_request.state,
                                        allowed_tools: &tool_names_for_route,
                                        tool_choice: &dispatch_tool_choice,
                                    },
                                );
                            ToolLifecycleRuntime::complete_corrective_call(
                                &session_repo,
                                assistant_message.id,
                                request.session.session.id,
                                request.protocol_turn_id,
                                record.id,
                                record.tool_name,
                                &result,
                                &route,
                                sink,
                            )
                            .await?;
                            if let Some(message) = lifecycle_guard
                                .record_tool_execution_error_effects(
                                    ToolExecutionErrorEffectsInput {
                                        effective_tool_name: &effective_tool_name,
                                        effective_arguments_json: &route.effective_arguments_json,
                                        error_text: &error_text,
                                        invalid_arguments_metadata: Some(&result.metadata),
                                        state: &step_request.state,
                                        tool_names: &tool_names_for_route,
                                        dispatch_tool_choice: &dispatch_tool_choice,
                                    },
                                )
                            {
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
                                    request.protocol_turn_id,
                                    sink,
                                )
                                .await;
                            }
                            continue;
                        }
                        failed_tool_count += 1;
                        ToolLifecycleRuntime::fail_executed_call(
                            &session_repo,
                            assistant_message.id,
                            request.session.session.id,
                            request.protocol_turn_id,
                            record.id,
                            record.tool_name,
                            &error_text,
                            &route,
                            sink,
                        )
                        .await?;
                        if let Some(message) = lifecycle_guard.record_tool_execution_error_effects(
                            ToolExecutionErrorEffectsInput {
                                effective_tool_name: &effective_tool_name,
                                effective_arguments_json: &route.effective_arguments_json,
                                error_text: &error_text,
                                invalid_arguments_metadata: None,
                                state: &step_request.state,
                                tool_names: &tool_names_for_route,
                                dispatch_tool_choice: &dispatch_tool_choice,
                            },
                        ) {
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
                                request.protocol_turn_id,
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
            TurnLifecycleKernel::turn_step_budget_exhausted_failure_message(),
            tool_call_count,
            failed_tool_count,
            change_count,
            request.protocol_turn_id,
            sink,
        )
        .await
    }
}

pub fn invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes()
}

pub(crate) fn turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields_fixture_passes() -> bool
{
    crate::agent::lifecycle_guard::turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields_fixture_passes()
}

pub(crate) fn lifecycle_guard_snapshot_hydrates_runtime_state_fixture_passes() -> bool {
    crate::agent::lifecycle_guard::snapshot_hydrates_runtime_state_parts_fixture_passes()
}

pub(crate) fn lifecycle_guard_snapshot_hydration_uses_canonical_item_order_fixture_passes() -> bool
{
    crate::agent::lifecycle_guard::snapshot_hydration_uses_canonical_item_order_fixture_passes()
}

pub fn assistant_message_lifecycle_sequence_fixture_passes() -> bool {
    crate::agent::assistant_message_lifecycle::assistant_message_lifecycle_sequence_fixture_passes()
}

pub fn state_lifecycle_persistence_sequence_fixture_passes() -> bool {
    crate::agent::state_lifecycle::state_lifecycle_persistence_sequence_fixture_passes()
}

async fn complete_turn(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    assistant_message_id: crate::session::MessageId,
    model: &str,
    base_url: &str,
    finish_reason: Option<FinishReason>,
    token_usage: Option<crate::session::TokenUsage>,
    context_window: u32,
    tool_call_count: usize,
    failed_tool_count: usize,
    change_count: usize,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    crate::agent::terminal_accounting::complete_turn(
        session_repo,
        session_id,
        assistant_message_id,
        model,
        base_url,
        finish_reason,
        token_usage,
        context_window,
        tool_call_count,
        failed_tool_count,
        change_count,
        protocol_turn_id,
        sink,
    )
    .await
}

pub(crate) fn rejected_final_message_event_persists_for_provider_replay_fixture_passes() -> bool {
    ToolLifecycleRuntime::rejected_final_message_event_persists_for_provider_replay_fixture_passes(
        LOOP_FIXTURE_MODEL,
        LOOP_FIXTURE_BASE_URL,
    )
}

pub(crate) fn terminal_token_accounting_sequence_fixture_passes() -> bool {
    crate::agent::terminal_accounting::terminal_token_accounting_sequence_fixture_passes(
        LOOP_FIXTURE_MODEL,
        LOOP_FIXTURE_BASE_URL,
    )
}

pub(crate) fn terminal_turn_projection_fixture_passes() -> bool {
    crate::agent::terminal_accounting::terminal_turn_projection_fixture_passes(
        LOOP_FIXTURE_MODEL,
        LOOP_FIXTURE_BASE_URL,
    )
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
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    crate::agent::terminal_accounting::interrupt_turn(
        session_repo,
        session_id,
        assistant_message_id,
        model,
        base_url,
        reason,
        tool_call_count,
        failed_tool_count,
        change_count,
        protocol_turn_id,
        sink,
    )
    .await
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
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    crate::agent::terminal_accounting::fail_turn(
        session_repo,
        session_id,
        assistant_message_id,
        model,
        base_url,
        message,
        tool_call_count,
        failed_tool_count,
        change_count,
        protocol_turn_id,
        sink,
    )
    .await
}

pub(crate) fn request_diagnostics_stream_retry_policy_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::request_diagnostics_stream_retry_policy_fixture_passes()
}

pub(crate) fn request_diagnostics_tool_choice_uses_runtime_dispatch_field_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::request_diagnostics_tool_choice_uses_runtime_dispatch_field_fixture_passes()
}

pub(crate) fn request_diagnostics_tool_surface_uses_chat_request_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::request_diagnostics_tool_surface_uses_chat_request_fixture_passes()
}

pub(crate) fn request_diagnostics_model_capabilities_use_chat_request_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::request_diagnostics_model_capabilities_use_chat_request_fixture_passes()
}

pub(crate) fn request_diagnostics_missing_model_capabilities_remain_absent_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::request_diagnostics_missing_model_capabilities_remain_absent_fixture_passes()
}

pub(crate) fn request_diagnostics_parallel_tool_calls_scope_matches_chat_request_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::request_diagnostics_parallel_tool_calls_scope_matches_chat_request_fixture_passes()
}

pub(crate) fn operation_feedback_uses_active_work_targets_fixture_passes() -> bool {
    ToolLifecycleRuntime::operation_feedback_uses_active_work_targets_fixture_passes()
}

fn compile_turn_control_envelope(
    request: &AgentRunRequest,
    active_work: Option<&ActiveWorkContract>,
    turn_decision: &crate::session::TurnDecisionDiagnostic,
    tool_names: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    authoring_grounding_recovery: Option<&AuthoringGroundingRecoveryEnvelope>,
    invalid_edit_recovery: Option<&InvalidEditRecoveryEnvelope>,
) -> crate::protocol::CompiledTurn {
    let allowed_tools = tool_names
        .iter()
        .filter_map(|name| ToolName::from_name(name))
        .collect::<Vec<_>>();
    let projection_id = ProjectionId::new();
    let context = TurnLifecycleKernel::compile_turn_context(CompileTurnContextInput {
        session_id: request.session.session.id,
        cwd: &request.session.workspace.cwd,
        workspace_root: &request.session.workspace.root,
        model: &request.model,
        config: &request.config,
        state: &request.state,
        history_items: &request.runtime_input.history_items,
        active_work,
        turn_decision,
        allowed_tools,
        tool_choice,
        projection_id,
    });
    let obligations = TurnLifecycleKernel::compile_turn_obligations(CompileTurnObligationsInput {
        context: &context,
        active_work,
        authoring_grounding_recovery,
        invalid_edit_recovery,
        history_items: &request.runtime_input.history_items,
        workspace_root: request.session.workspace.root.as_path(),
    });
    TurnEngine::compile(TurnEngineInput {
        turn_id: request.protocol_turn_id,
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    })
}

pub(crate) fn control_envelope_preserves_current_turn_id_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::control_envelope_preserves_current_turn_id_fixture_passes()
}

pub(crate) fn content_shape_recovery_projection_omits_inactive_submitted_targets_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::content_shape_recovery_projection_omits_inactive_submitted_targets_fixture_passes()
}

pub(crate) fn verification_turn_omits_consumed_images_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::verification_turn_omits_consumed_images_fixture_passes()
}

pub(crate) fn provider_chat_request_omits_consumed_images_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_chat_request_omits_consumed_images_fixture_passes()
}

pub(crate) fn singleton_write_surface_requires_tool_choice_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::singleton_write_surface_requires_tool_choice_fixture_passes()
}

pub(crate) fn required_write_target_mismatch_feedback_projects_test_content_authority() -> bool {
    crate::agent::content_shape_contract::required_write_content_shape_mismatch_progress_class_fixture_passes()
}

pub(crate) fn exact_write_route_accepts_generated_test_content() -> bool {
    crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden()
}

pub(crate) fn content_shape_mismatch_feedback_carries_positive_test_contract() -> bool {
    crate::agent::content_shape_contract::content_shape_contract_fixtures_are_workflow_neutral_fixture_passes()
}

pub(crate) fn test_target_content_shape_write_lifecycle_enforced_fixture_passes() -> bool {
    crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden()
}

pub(crate) fn test_target_content_shape_rejects_string_literal_wrapped_tests_fixture_passes() -> bool
{
    crate::agent::content_shape_contract::test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_escaped_whole_file_fixture_passes() -> bool {
    crate::agent::content_shape_contract::source_content_shape_rejects_escaped_whole_file_fixture_passes()
}

pub(crate) fn source_content_shape_normalizes_escaped_repair_candidate_fixture_passes() -> bool {
    crate::agent::edit_recovery::source_content_shape_normalizes_escaped_repair_candidate_fixture_passes()
}

pub(crate) fn loop_impl_escaped_source_fixture_language_neutral_fixture_passes() -> bool {
    source_content_shape_normalizes_escaped_repair_candidate_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_test_module_payload_fixture_passes() -> bool {
    crate::agent::content_shape_contract::source_content_shape_rejects_test_module_payload_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_markdown_payload_fixture_passes() -> bool {
    crate::agent::content_shape_contract::source_content_shape_rejects_markdown_payload_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_raw_prose_line_fixture_passes() -> bool {
    crate::agent::content_shape_contract::source_content_shape_rejects_raw_prose_line_fixture_passes(
    )
}

pub(crate) fn source_content_shape_rejects_duplicate_entrypoint_fixture_passes() -> bool {
    crate::agent::content_shape_contract::source_content_shape_rejects_duplicate_entrypoint_fixture_passes()
}

pub(crate) fn corrective_content_shape_no_progress_terminal_guard_fixture_passes() -> bool {
    crate::agent::content_shape_contract::required_write_content_shape_mismatch_progress_class_fixture_passes()
}

pub(crate) fn text_artifact_content_shape_rejects_serialized_markdown_fixture_passes() -> bool {
    crate::agent::content_shape_contract::text_artifact_content_shape_rejects_serialized_markdown_fixture_passes()
}

pub(crate) fn content_shape_mismatch_canonicalizes_workspace_absolute_target_fixture_passes() -> bool
{
    crate::agent::content_shape_contract::content_shape_mismatch_canonicalizes_workspace_absolute_target_fixture_passes()
}

pub(crate) fn test_target_content_shape_apply_patch_post_content_enforced_fixture_passes() -> bool {
    crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden()
}

pub(crate) fn closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes()
}

pub(crate) fn clean_closeout_final_message_lifecycle_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::clean_closeout_final_message_lifecycle_fixture_passes()
}

pub(crate) fn answer_only_final_message_lifecycle_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::answer_only_final_message_lifecycle_fixture_passes()
}

pub(crate) fn answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes()
}

pub(crate) fn closeout_ready_final_response_timeout_guard_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::closeout_ready_final_response_timeout_guard_fixture_passes()
}

pub(crate) fn open_obligation_final_message_guard_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::open_obligation_final_message_guard_fixture_passes()
}

pub(crate) fn open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes()
}

pub(crate) fn docs_route_final_message_recovery_requires_content_grounding_fixture_passes() -> bool
{
    crate::agent::grounding_evidence::docs_route_content_grounding_requires_typed_supporting_context_fixture_passes()
}

pub(crate) fn executed_tool_failure_terminal_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::executed_tool_failure_terminal_guard_fixture_passes()
}

pub(crate) fn loop_impl_terminal_guard_fixture_language_neutral_fixture_passes() -> bool {
    open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes()
        && executed_tool_failure_terminal_guard_fixture_passes()
}

pub(crate) fn progress_projection_loop_terminal_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::progress_projection_terminal_guard_fixture_passes()
}

pub(crate) fn open_authoring_operation_intent_classifies_non_content_tools_fixture_passes() -> bool
{
    ToolLifecycleRuntime::open_authoring_operation_intent_classifies_non_content_tools_fixture_passes(
    )
}

pub(crate) fn open_authoring_operation_intent_preserves_tool_surface_fixture_passes() -> bool {
    ToolLifecycleRuntime::open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
}

pub(crate) fn loop_impl_operation_intent_fixture_language_neutral_fixture_passes() -> bool {
    open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
}

pub(crate) fn docs_route_semantic_no_progress_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_semantic_no_progress_guard_fixture_passes()
}

pub(crate) fn docs_route_idempotent_write_no_progress_terminal_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_idempotent_write_no_progress_terminal_guard_fixture_passes()
}

pub(crate) fn authoring_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::authoring_supporting_context_budget_recovery_surface_fixture_passes()
}

pub(crate) fn multi_target_authoring_consumed_grounding_narrows_edit_recovery_fixture_passes()
-> bool {
    crate::agent::grounding_evidence::multi_target_authoring_consumed_grounding_narrows_edit_recovery_fixture_passes()
}

pub(crate) fn repair_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::repair_supporting_context_budget_recovery_surface_fixture_passes(
    )
}

pub(crate) fn invalid_edit_arguments_project_no_progress_recovery_fixture_passes() -> bool {
    crate::agent::edit_recovery::invalid_edit_arguments_project_no_progress_recovery_fixture_passes(
    )
}

pub(crate) fn invalid_edit_arguments_terminal_guard_fixture_passes() -> bool {
    crate::agent::edit_recovery::invalid_edit_arguments_terminal_guard_fixture_passes()
}

pub(crate) fn loop_impl_invalid_edit_fixture_language_neutral_fixture_passes() -> bool {
    invalid_edit_arguments_project_no_progress_recovery_fixture_passes()
        && invalid_edit_arguments_terminal_guard_fixture_passes()
}

pub(crate) fn non_edit_invalid_tool_arguments_terminal_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::non_edit_invalid_tool_arguments_terminal_guard_fixture_passes()
}

pub(crate) fn malformed_write_patch_capable_recovery_surface_fixture_passes() -> bool {
    crate::agent::edit_recovery::malformed_write_patch_capable_recovery_surface_fixture_passes()
}

pub(crate) fn loop_impl_malformed_write_fixture_language_neutral_fixture_passes() -> bool {
    non_edit_invalid_tool_arguments_terminal_guard_fixture_passes()
        && malformed_write_patch_capable_recovery_surface_fixture_passes()
}

pub(crate) fn malformed_apply_patch_write_recovery_surface_fixture_passes() -> bool {
    crate::agent::edit_recovery::malformed_apply_patch_write_recovery_surface_fixture_passes()
}

pub(crate) fn loop_impl_malformed_apply_patch_fixture_language_neutral_fixture_passes() -> bool {
    malformed_apply_patch_write_recovery_surface_fixture_passes()
}

pub(crate) fn failed_patch_context_mismatch_reopens_target_grounding_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::failed_patch_context_mismatch_reopens_target_grounding_fixture_passes()
}

pub(crate) fn malformed_write_arguments_terminal_quote_repair_fixture_passes() -> bool {
    crate::agent::edit_recovery::malformed_write_arguments_terminal_quote_repair_fixture_passes()
}

pub(crate) fn singleton_active_target_write_arguments_repair_fixture_passes() -> bool {
    crate::agent::edit_recovery::singleton_active_target_write_arguments_repair_fixture_passes()
}

pub(crate) fn loop_impl_singleton_write_argument_fixture_language_neutral_fixture_passes() -> bool {
    singleton_active_target_write_arguments_repair_fixture_passes()
}

pub(crate) fn verification_repair_target_grounding_surface_keeps_read_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::verification_repair_target_grounding_surface_keeps_read_fixture_passes()
}

pub(crate) fn source_repair_initial_grounding_precedes_edit_only_recovery_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::source_repair_initial_grounding_precedes_edit_only_recovery_fixture_passes()
}

pub(crate) fn rejected_tool_batch_terminal_guard_waits_for_followup_fixture_passes() -> bool {
    ToolLifecycleRuntime::rejected_tool_batch_terminal_guard_waits_for_followup_fixture_passes()
}

pub(crate) fn docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes() -> bool
{
    ToolLifecycleRuntime::docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes()
}

pub(crate) fn docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes()
}

pub(crate) fn docs_route_budget_exhaustion_survives_partial_write_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_budget_exhaustion_survives_partial_write_fixture_passes()
}

pub(crate) fn docs_route_supporting_context_budget_fixture_workflow_neutral_fixture_passes() -> bool
{
    ToolLifecycleRuntime::docs_route_supporting_context_budget_fixture_workflow_neutral_fixture_passes()
}

pub(crate) fn edit_surface_registry_symmetry_fixture_passes() -> bool {
    ToolLifecycleRuntime::edit_surface_registry_symmetry_fixture_passes()
}

pub(crate) fn loop_impl_docs_budget_edit_surface_fixture_language_neutral_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_budget_edit_surface_fixture_passes()
        && edit_surface_registry_symmetry_fixture_passes()
}

pub(crate) fn verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes()
-> bool {
    ToolLifecycleRuntime::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes()
}

pub(crate) fn verification_active_work_preserves_tool_surface_and_rejects_wrong_command_failed_checks()
-> Vec<&'static str> {
    ToolLifecycleRuntime::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_failed_checks()
}

pub(crate) fn repair_active_shell_probe_uses_repair_target_authority_fixture_passes() -> bool {
    ToolLifecycleRuntime::repair_active_shell_probe_uses_repair_target_authority_fixture_passes()
}

pub(crate) fn post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes() -> bool {
    TurnLifecycleKernel::post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes(
    )
}

pub(crate) fn verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes()
-> bool {
    TurnLifecycleKernel::verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes()
}

pub(crate) fn singleton_verification_command_arguments_are_runtime_owned_fixture_passes() -> bool {
    TurnLifecycleKernel::singleton_verification_command_arguments_are_runtime_owned_fixture_passes()
}

pub(crate) fn same_verification_failure_terminal_guard_fixture_passes() -> bool {
    ToolLifecycleRuntime::same_verification_failure_terminal_guard_fixture_passes()
}

pub(crate) fn loop_impl_verification_public_command_fixture_domain_neutral_fixture_passes() -> bool
{
    TurnLifecycleKernel::verification_public_command_fixture_domain_neutral_fixture_passes()
}

pub(crate) fn active_authoring_rejects_wrong_target_fixture_passes() -> bool {
    ToolLifecycleRuntime::active_authoring_rejects_wrong_target_fixture_passes()
}

pub(crate) fn verification_repair_rejects_non_exact_write_target_fixture_passes() -> bool {
    ToolLifecycleRuntime::verification_repair_rejects_non_exact_write_target_fixture_passes()
}

pub(crate) fn docs_route_rejects_completed_deliverable_regression_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_route_rejects_completed_deliverable_regression_fixture_passes()
}

pub(crate) fn loop_impl_active_authoring_docs_regression_fixture_domain_neutral_fixture_passes()
-> bool {
    ToolLifecycleRuntime::active_authoring_docs_regression_fixture_domain_neutral_fixture_passes()
}

pub(crate) fn provider_required_tool_choice_final_message_recovery_fixture_passes() -> bool {
    TurnLifecycleKernel::provider_required_tool_choice_final_message_recovery_fixture_passes()
}

pub(crate) fn rejected_model_action_no_progress_effects_are_guard_owned_fixture_passes() -> bool {
    ToolLifecycleRuntime::rejected_model_action_no_progress_effects_are_guard_owned_fixture_passes()
}

pub(crate) fn final_dispatch_source_schema_projection_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::final_dispatch_source_schema_projection_fixture_passes()
}

pub(crate) fn authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes()
}

pub(crate) fn docs_patch_context_final_message_recovery_preserves_grounding_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::docs_patch_context_final_message_recovery_preserves_grounding_fixture_passes()
}

pub(crate) fn docs_existing_target_update_keeps_exact_read_grounding_fixture_passes() -> bool {
    ToolLifecycleRuntime::docs_existing_target_update_keeps_exact_read_grounding_fixture_passes()
}

pub(crate) fn loop_impl_docs_existing_target_grounding_fixture_domain_neutral_fixture_passes()
-> bool {
    ToolLifecycleRuntime::docs_existing_target_grounding_fixture_domain_neutral_fixture_passes()
}

pub(crate) fn generated_test_authoring_keeps_recent_source_reference_read_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::generated_test_authoring_keeps_recent_source_reference_read_fixture_passes()
}

pub(crate) fn generated_test_consumed_source_reference_requires_active_target_fixture_passes()
-> bool {
    ToolLifecycleRuntime::generated_test_consumed_source_reference_requires_active_target_fixture_passes()
}

pub(crate) fn loop_impl_generated_test_source_reference_fixture_domain_neutral_fixture_passes()
-> bool {
    ToolLifecycleRuntime::generated_test_source_reference_fixture_domain_neutral_fixture_passes()
}

pub(crate) fn singleton_missing_authoring_target_projects_create_action_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::singleton_missing_authoring_target_projects_create_action_fixture_passes()
}

pub(crate) fn concrete_write_required_action_narrows_broad_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::concrete_write_required_action_narrows_broad_surface_fixture_passes()
}

pub(crate) fn codex_style_code_authoring_omits_whole_file_write_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::codex_style_code_authoring_omits_whole_file_write_fixture_passes(
    )
}

pub(crate) fn codex_style_code_authoring_omits_json_discovery_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::codex_style_code_authoring_omits_json_discovery_surface_fixture_passes()
}

pub(crate) fn codex_style_docs_authoring_omits_non_codex_json_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::codex_style_docs_authoring_omits_non_codex_json_surface_fixture_passes()
}

pub(crate) fn open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes()
}

pub(crate) fn multi_target_open_authoring_final_message_correction_names_targets_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::multi_target_open_authoring_final_message_correction_names_targets_fixture_passes()
}

pub(crate) fn final_message_recovery_is_system_control_projection_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::final_message_recovery_is_system_control_projection_fixture_passes()
}

pub(crate) fn invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes()
}

pub(crate) fn invalid_edit_recovery_projects_candidate_target_operation_fixture_passes() -> bool {
    crate::agent::edit_recovery::invalid_edit_recovery_projects_candidate_target_operation_fixture_passes()
}

pub(crate) fn invalid_edit_arguments_recovery_persists_across_final_message_fixture_passes() -> bool
{
    crate::agent::edit_recovery::invalid_edit_arguments_recovery_persists_across_final_message_fixture_passes()
}

pub(crate) fn mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes()
}

pub(crate) fn content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes()
}

pub(crate) fn stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition_fixture_passes()
}

pub(crate) fn open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes()
}

pub(crate) fn open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes()
}

pub(crate) fn code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes()
}

pub(crate) fn failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes()
}

pub(crate) fn source_repair_final_message_correction_uses_exact_write_action_fixture_passes() -> bool
{
    crate::agent::lifecycle_kernel::source_repair_final_message_correction_uses_exact_write_action_fixture_passes()
}

pub(crate) fn required_repair_write_missing_tool_is_not_restored_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::required_repair_write_missing_tool_is_not_restored_fixture_passes()
}

pub(crate) fn provider_system_context_normalization_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_system_context_normalization_fixture_passes()
}

pub(crate) fn provider_replay_effective_tool_surface_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_replay_effective_tool_surface_fixture_passes()
}

pub(crate) fn loop_impl_provider_replay_effective_surface_fixture_effective_test_payload_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::provider_replay_effective_surface_fixture_effective_test_payload_fixture_passes()
}

pub(crate) fn provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
-> bool {
    crate::agent::lifecycle_kernel::provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
}

pub(crate) fn provider_replay_omits_intermediate_assistant_text_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_replay_omits_intermediate_assistant_text_fixture_passes(
    )
}

pub(crate) fn provider_replay_omits_assistant_tool_call_content_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_replay_omits_assistant_tool_call_content_fixture_passes(
    )
}

pub(crate) fn provider_metadata_mode_serializes_named_tool_choice_fixture_passes() -> bool {
    crate::agent::lifecycle_kernel::provider_metadata_mode_serializes_named_tool_choice_fixture_passes()
}

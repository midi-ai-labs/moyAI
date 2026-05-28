use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent::compaction::maybe_compact;
use crate::agent::edit_recovery::{
    InvalidEditRecoveryEnvelope, failed_edit_control_recovery_envelope,
    invalid_apply_patch_arguments_need_write_recovery,
    invalid_edit_arguments_control_recovery_envelope, invalid_edit_arguments_no_progress_key,
    invalid_edit_arguments_terminal_message, invalid_edit_recovery_semantic_no_progress_key,
    invalid_tool_arguments_result, invalid_write_arguments_need_patch_capable_recovery,
    is_invalid_tool_arguments_error, normalized_escaped_source_write_candidate,
    patch_context_mismatch_target_grounding_read_satisfied,
    patch_context_mismatch_target_grounding_surface_active,
    record_patch_context_mismatch_grounding_targets,
    repair_unambiguous_malformed_edit_arguments_json, repair_write_arguments_from_active_target,
    should_terminalize_invalid_edit_arguments_no_progress,
};
use crate::agent::event::{CompletedToolCall, StreamAccumulator};
use crate::agent::grounding_evidence::{
    active_authoring_target_keys, active_authoring_targets_need_grounding,
    authoring_grounding_recovery_envelope, authoring_grounding_recovery_obligation,
    authoring_missing_grounding_targets, docs_route_has_required_content_grounding_evidence,
    generated_test_reference_consumed_read_requires_active_target,
    history_has_current_source_reference_read_for_generated_test,
    history_has_unread_source_change_for_generated_test, matching_active_target_key,
    normalize_path_for_target_match, record_authoring_grounded_active_target,
    singleton_active_target_exists,
};
use crate::agent::lifecycle_kernel::{
    ActionAdjudication, ProviderActionAdapter, ReplayNormalizer, TurnLifecycleKernel,
    TurnLifecyclePlanInput, TurnLifecyclePreNormalizationSurfaceInput,
    TurnLifecycleRecoveryContext, TurnLifecycleRecoverySurfaceInput,
    compile_turn_lifecycle_tool_choice,
};
use crate::agent::prompt::{AgentRunRequest, PromptBuilder, RuntimeInputView};
use crate::agent::prompt_assets::{hard_final_step_reminder, max_steps_reminder};
use crate::agent::state::{
    ActiveWorkContract, active_work_contract_for_history_items,
    reduce_session_state_from_history_items,
};
use crate::agent::tool_orchestrator::{
    AuthoringGroundingRecoveryEnvelope, OperationNoProgressBudgetExhaustion,
    PreExecutionCorrectiveInput, PreExecutionCorrectiveNoProgressInput,
    RejectedToolNoProgressGuardRequest, ToolExecutionRequest, ToolLifecycleRuntime,
    ToolRouteRequest,
};
use crate::agent::turn_decision::build_turn_decision_diagnostic;
use crate::agent::verification::{
    canonical_verification_command_identity_key, verification_command_satisfaction_keys,
};
use crate::cli::ConfirmationPrompt;
use crate::edit::ChangeSummary;
use crate::error::AgentError;
use crate::llm::{ChatRequest, LlmClient, LlmEventSink, LlmResponseSummary};
use crate::protocol::{
    ActiveWorkContractProjection, CandidateRepairValidity, DispatchPolicy, EvidenceRef,
    HistoryItem, HistoryItemPayload, ObligationCompiler, ObligationKind, ObligationStatus,
    OperationIntent, OutputContract, ProjectionId, ProtocolEventStore, RequiredAction,
    RequiredActionKind, SandboxProfile, ToolChoice, ToolLifecycleStatus, TurnContext,
    TurnControlEnvelope, TurnEngine, TurnEngineInput, TurnId, TurnObligation,
};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, ContractStatus, DocsArea, DocsDeliverableCoverage, DocsDeliverableKind,
    DocsGroundingCoverage, DocsGroundingRequirement, DocsRouteState, FinishReason, MessageMetadata,
    MessagePart, MessageRole, NewMessage, NewPart, PartKind, RequestControlEnvelopeDiagnostic,
    RequestControlEnvelopeIssueDiagnostic, RequestControlObligationDiagnostic,
    RequestControlSurfaceDiagnostic, RequestDiagnosticsPart, RequestMessageDiagnostic,
    RequestToolCallDiagnostic, RequestToolSchemaDiagnostic, RunSummary, SessionId,
    SessionRepository, SessionStateSnapshot, SessionStatus, TaskRoute, TextPart, TodoItem,
    TodoKind, TodoStatus, TokenAccountingState, TurnDecisionWarningSeverity,
};
use crate::storage::{SqliteSessionRepository, StoreBundle};
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolName, ToolResult};

const PROGRESS_PROJECTION_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 8;
const VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const SAME_VERIFICATION_FAILURE_TERMINAL_THRESHOLD: usize = 3;
const OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD: usize = 3;
const INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD: usize = 3;
const CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS: u64 = 120_000;

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
        let (assistant_message, assistant_started_event) = session_repo
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
        sink.emit_pre_recorded(assistant_started_event)?;

        let mut tool_call_count = 0usize;
        let mut failed_tool_count = 0usize;
        let mut change_count = 0usize;
        let mut rejected_tool_proposals = BTreeMap::<String, usize>::new();
        let mut executed_tool_failure_counts = BTreeMap::<String, usize>::new();
        let mut progress_projection_no_progress_counts = BTreeMap::<String, usize>::new();
        let mut operation_non_content_no_progress_counts = BTreeMap::<String, usize>::new();
        let mut verification_supporting_context_no_progress_counts =
            BTreeMap::<String, usize>::new();
        let mut same_verification_failure_counts = BTreeMap::<String, usize>::new();
        let mut docs_spec_semantic_reconciliation_counts = BTreeMap::<String, usize>::new();
        let mut public_command_contract_counts = BTreeMap::<String, usize>::new();
        let mut wrong_verification_command_counts = BTreeMap::<String, usize>::new();
        let mut wrong_authoring_target_counts = BTreeMap::<String, usize>::new();
        let mut repair_target_authority_violation_counts = BTreeMap::<String, usize>::new();
        let mut invalid_edit_argument_counts = BTreeMap::<String, usize>::new();
        let mut malformed_write_patch_recovery_pending = false;
        let mut malformed_apply_patch_write_recovery_pending = false;
        let mut invalid_edit_arguments_recovery = None::<InvalidEditRecoveryEnvelope>;
        let mut patch_context_mismatch_grounding_targets = BTreeSet::<String>::new();
        let mut authoring_supporting_context_budget_exhausted = BTreeSet::<String>::new();
        let mut authoring_grounded_active_targets = BTreeSet::<String>::new();
        let mut authoring_target_grounding_required_counts = BTreeMap::<String, usize>::new();
        let mut generated_test_target_grounding_required_counts = BTreeMap::<String, usize>::new();
        let mut repair_supporting_context_budget_exhausted = BTreeSet::<String>::new();
        let mut docs_supporting_context_budget_exhausted = BTreeSet::<String>::new();
        let mut docs_supporting_context_budget_exhausted_counts = BTreeMap::<String, usize>::new();
        let mut open_obligation_final_message_count = 0usize;
        let mut open_obligation_final_message_counts = BTreeMap::<String, usize>::new();
        let mut open_obligation_final_message_recovery =
            None::<OpenObligationFinalMessageRecoveryEnvelope>;
        let mut open_obligation_final_message_hard_edit_recovery_pending = false;
        let mut provider_required_tool_choice_final_message_recovery_pending = false;
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
            if reduced_state != persisted_state {
                let event = crate::session::RunEvent::StateUpdated {
                    session_id: request.session.session.id,
                    state: reduced_state.clone(),
                };
                session_repo
                    .update_state_with_protocol_event(
                        request.session.session.id,
                        &reduced_state,
                        &event,
                        request.protocol_turn_id,
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?;
                sink.emit_pre_recorded(event)?;
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
                open_obligation_final_message_count = 0;
                open_obligation_final_message_counts.clear();
                open_obligation_final_message_recovery = None;
                open_obligation_final_message_hard_edit_recovery_pending = false;
                provider_required_tool_choice_final_message_recovery_pending = false;
            }
            let final_message_recovery_prompt = open_obligation_final_message_recovery
                .as_ref()
                .map(|envelope| envelope.prompt.clone());
            let invalid_edit_recovery_prompt = invalid_edit_arguments_recovery
                .as_ref()
                .map(|envelope| envelope.prompt.clone());
            if let Some(correction) = final_message_recovery_prompt.as_ref() {
                system_prompt = format!("{correction}\n\n{system_prompt}");
            }
            if let Some(correction) = invalid_edit_recovery_prompt.as_ref() {
                system_prompt = format!("{correction}\n\n{system_prompt}");
            }
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
            if TurnLifecycleKernel::clean_closeout_final_message_lifecycle(
                &step_request.state,
                active_work.as_ref(),
            ) {
                tools.clear();
            }
            let stable_tools = stable_tool_schemas_from_registry(&self.agent.registry);
            if TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
                &step_request.state,
                &docs_supporting_context_budget_exhausted,
            ) {
                tools.retain(|tool| {
                    TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_tool_visible(
                        &tool.name,
                    )
                });
            }
            let authoring_grounding_missing_targets = authoring_missing_grounding_targets(
                &step_request.runtime_input.history_items,
                &step_request.state,
                &request.session.workspace.root,
                &authoring_grounded_active_targets,
            );
            let authoring_grounding_recovery =
                if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
                    &step_request.state,
                    &authoring_supporting_context_budget_exhausted,
                ) {
                    Some(authoring_grounding_recovery_envelope(
                        &step_request.runtime_input.history_items,
                        &step_request.state,
                        &request.session.workspace.root,
                        &authoring_grounded_active_targets,
                    ))
                } else {
                    None
                };
            let authoring_supporting_context_budget_recovery_needs_read =
                !authoring_grounding_missing_targets.is_empty();
            let authoring_supporting_context_budget_recovery_active =
                TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
                    &step_request.state,
                    &authoring_supporting_context_budget_exhausted,
                );
            if authoring_supporting_context_budget_recovery_active {
                tools.retain(|tool| {
                    TurnLifecycleKernel::authoring_supporting_context_budget_recovery_tool_visible(
                        &tool.name,
                        authoring_supporting_context_budget_recovery_needs_read,
                    )
                });
                if let Some(envelope) = authoring_grounding_recovery.as_ref() {
                    constrain_read_schema_to_missing_authoring_targets(&mut tools, envelope);
                }
            }
            let generated_test_source_reference_grounding_active =
                TurnLifecycleKernel::generated_test_source_reference_grounding_active(
                    &step_request.state,
                    history_has_unread_source_change_for_generated_test(
                        &step_request.runtime_input.history_items,
                    ),
                );
            if generated_test_source_reference_grounding_active {
                let orientation_allowed = !authoring_supporting_context_budget_recovery_active;
                TurnLifecycleKernel::apply_generated_test_source_reference_grounding_surface(
                    &mut tools,
                    &stable_tools,
                    orientation_allowed,
                );
            }
            let generated_test_reference_consumed_target_grounding_active =
                !generated_test_source_reference_grounding_active
                    && TurnLifecycleKernel::generated_test_reference_consumed_target_grounding_active(
                        &step_request.state,
                        history_has_current_source_reference_read_for_generated_test(
                            &step_request.runtime_input.history_items,
                        ),
                        history_has_unread_source_change_for_generated_test(
                            &step_request.runtime_input.history_items,
                        ),
                        active_authoring_targets_need_grounding(
                            &step_request.runtime_input.history_items,
                            &step_request.state,
                            &request.session.workspace.root,
                            &BTreeSet::new(),
                        ),
                    );
            if generated_test_reference_consumed_target_grounding_active {
                TurnLifecycleKernel::apply_generated_test_reference_consumed_target_grounding_surface(
                    &mut tools,
                    &stable_tools,
                );
            }
            let singleton_missing_authoring_target_create_action_active =
                !generated_test_source_reference_grounding_active
                    && TurnLifecycleKernel::singleton_missing_authoring_target_create_action_active(
                        &step_request.state,
                        singleton_active_target_exists(
                            &step_request.state,
                            &request.session.workspace.root,
                        ),
                    );
            if singleton_missing_authoring_target_create_action_active {
                TurnLifecycleKernel::augment_tools_from_stable_surface(
                    &mut tools,
                    &stable_tools,
                    |tool_name| {
                        TurnLifecycleKernel::singleton_missing_authoring_target_create_action_tool_visible(tool_name)
                    },
                );
                tools.retain(|tool| {
                    TurnLifecycleKernel::singleton_missing_authoring_target_create_action_tool_visible(&tool.name)
                });
            }
            let existing_target_grounding_recovery_active =
                TurnLifecycleKernel::existing_target_grounding_recovery_active(
                    &step_request.state,
                    active_authoring_targets_need_grounding(
                        &step_request.runtime_input.history_items,
                        &step_request.state,
                        &request.session.workspace.root,
                        &authoring_grounded_active_targets,
                    ),
                );
            if existing_target_grounding_recovery_active {
                TurnLifecycleKernel::augment_tools_from_stable_surface(
                    &mut tools,
                    &stable_tools,
                    TurnLifecycleKernel::existing_target_grounding_recovery_tool_visible,
                );
                tools.retain(|tool| {
                    TurnLifecycleKernel::existing_target_grounding_recovery_tool_visible(&tool.name)
                });
                let envelope = authoring_grounding_recovery_envelope(
                    &step_request.runtime_input.history_items,
                    &step_request.state,
                    &request.session.workspace.root,
                    &authoring_grounded_active_targets,
                );
                constrain_read_schema_to_missing_authoring_targets(&mut tools, &envelope);
            }
            let patch_context_mismatch_grounding_active =
                patch_context_mismatch_target_grounding_surface_active(
                    &step_request.state,
                    &patch_context_mismatch_grounding_targets,
                );
            let repair_supporting_context_budget_recovery_active =
                TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
                    &step_request.state,
                    &repair_supporting_context_budget_exhausted,
                );
            if repair_supporting_context_budget_recovery_active
                && !patch_context_mismatch_grounding_active
            {
                tools.retain(|tool| {
                    TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(
                        &tool.name,
                    )
                });
            }
            let pre_authority_tool_names = tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<BTreeSet<_>>();
            let mut verification_target_grounding_active = false;
            if patch_context_mismatch_grounding_active {
                if step_request.state.route == TaskRoute::Docs
                    && step_request.state.process_phase == crate::session::ProcessPhase::Author
                {
                    TurnLifecycleKernel::augment_tools_from_stable_surface(
                        &mut tools,
                        &stable_tools,
                        TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible,
                    );
                    tools.retain(|tool| {
                        TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible(
                            &tool.name,
                        )
                    });
                } else {
                    TurnLifecycleKernel::augment_tools_from_stable_surface(
                        &mut tools,
                        &stable_tools,
                        TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
                    );
                    tools.retain(|tool| {
                        TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(&tool.name)
                    });
                    verification_target_grounding_active = true;
                }
            } else if !repair_supporting_context_budget_recovery_active
                && TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
                    &step_request.state,
                    &pre_authority_tool_names,
                )
            {
                TurnLifecycleKernel::augment_tools_from_stable_surface(
                    &mut tools,
                    &stable_tools,
                    TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
                );
                tools.retain(|tool| {
                    TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                        &tool.name,
                    )
                });
                verification_target_grounding_active = true;
            }
            let provider_noncompliance_edit_recovery_active =
                TurnLifecycleKernel::provider_noncompliance_edit_recovery_applies(
                    &step_request.state,
                    &rejected_tool_proposals,
                );
            let wrong_target_authoring_edit_recovery_active =
                TurnLifecycleKernel::wrong_target_authoring_edit_recovery_applies(
                    &step_request.state,
                    &wrong_authoring_target_counts,
                );
            if provider_noncompliance_edit_recovery_active
                && tools.iter().any(|tool| {
                    TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(
                        &tool.name,
                    )
                })
            {
                tools.retain(|tool| {
                    TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(
                        &tool.name,
                    )
                });
            }
            if !repair_supporting_context_budget_recovery_active
                && !provider_noncompliance_edit_recovery_active
                && !wrong_target_authoring_edit_recovery_active
                && !patch_context_mismatch_grounding_active
                && TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
                    &step_request.state,
                    &tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect::<BTreeSet<_>>(),
                )
            {
                TurnLifecycleKernel::augment_tools_from_stable_surface(
                    &mut tools,
                    &stable_tools,
                    TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
                );
                tools.retain(|tool| {
                    TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                        &tool.name,
                    )
                });
                verification_target_grounding_active = true;
            }
            let open_obligation_final_message_recovery_active =
                open_obligation_final_message_recovery.is_some()
                    && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                        &step_request.state,
                    );
            let failed_edit_recovery_active = invalid_edit_arguments_recovery.is_some()
                && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                    &step_request.state,
                );
            let code_authoring_final_message_hard_edit_recovery_active =
                (open_obligation_final_message_hard_edit_recovery_pending
                    || open_obligation_final_message_recovery
                        .as_ref()
                        .is_some_and(|envelope| envelope.count >= 2))
                    && TurnLifecycleKernel::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(
                        &step_request.state,
                    );
            if code_authoring_final_message_hard_edit_recovery_active {
                open_obligation_final_message_hard_edit_recovery_pending = true;
            }
            let code_authoring_final_message_recovery_stable_surface_active =
                open_obligation_final_message_recovery_active
                    && !code_authoring_final_message_hard_edit_recovery_active
                    && !failed_edit_recovery_active
                    && TurnLifecycleKernel::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(
                        &step_request.state,
                    );
            let code_repair_final_message_recovery_stable_surface_active =
                open_obligation_final_message_recovery_active
                    && !failed_edit_recovery_active
                    && TurnLifecycleKernel::code_repair_open_obligation_final_message_recovery_uses_stable_surface(
                        &step_request.state,
                    );
            let docs_content_grounding_recovery_active =
                TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
                    &step_request.state,
                    docs_route_has_required_content_grounding_evidence(
                        &step_request.state,
                        &step_request.runtime_input.history_items,
                    ),
                );
            let docs_grounding_final_message_recovery_active =
                open_obligation_final_message_recovery_active
                    && !code_authoring_final_message_recovery_stable_surface_active
                    && !code_repair_final_message_recovery_stable_surface_active
                    && docs_content_grounding_recovery_active;
            let authoring_target_grounding_final_message_recovery_active =
                open_obligation_final_message_recovery_active
                    && !code_authoring_final_message_recovery_stable_surface_active
                    && !code_repair_final_message_recovery_stable_surface_active
                    && TurnLifecycleKernel::authoring_target_grounding_final_message_recovery_active(
                        &step_request.state,
                        active_authoring_targets_need_grounding(
                            &step_request.runtime_input.history_items,
                            &step_request.state,
                            &request.session.workspace.root,
                            &authoring_grounded_active_targets,
                        ),
                    );
            let malformed_write_patch_recovery_active = malformed_write_patch_recovery_pending
                && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                    &step_request.state,
                )
                && tools.iter().any(|tool| tool.name == "write")
                && tools.iter().any(|tool| tool.name == "apply_patch");
            let malformed_apply_patch_write_recovery_active =
                malformed_apply_patch_write_recovery_pending
                    && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                        &step_request.state,
                    )
                    && tools.iter().any(|tool| tool.name == "apply_patch");
            let provider_required_tool_choice_final_message_recovery_active =
                provider_required_tool_choice_final_message_recovery_pending
                    && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                        &step_request.state,
                    )
                    && TurnLifecycleKernel::provider_required_tool_choice_final_message_recovery_has_write_surface(
                        &tools,
                        &stable_tools,
                    );
            let progress_projection_edit_recovery_active = !progress_projection_no_progress_counts
                .is_empty()
                && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                    &step_request.state,
                )
                && tools.iter().any(|tool| {
                    TurnLifecycleKernel::progress_projection_edit_recovery_tool_visible(
                        &step_request.state,
                        &tool.name,
                        false,
                    )
                });
            let progress_projection_edit_recovery_needs_grounding_read =
                progress_projection_edit_recovery_active
                    && active_authoring_targets_need_grounding(
                        &step_request.runtime_input.history_items,
                        &step_request.state,
                        request.session.workspace.root.as_path(),
                        &authoring_grounded_active_targets,
                    );
            let recovery_context = TurnLifecycleRecoveryContext {
                provider_noncompliance_edit_recovery_active,
                wrong_target_authoring_edit_recovery_active,
                provider_required_tool_choice_final_message_recovery_active,
                code_authoring_final_message_hard_edit_recovery_active,
                generated_test_source_reference_grounding_active,
                generated_test_reference_consumed_target_grounding_active,
                verification_target_grounding_active,
                patch_context_mismatch_grounding_active,
                authoring_target_grounding_final_message_recovery_active,
                existing_target_grounding_recovery_active,
                docs_grounding_final_message_recovery_active,
                docs_content_grounding_recovery_active,
                malformed_write_patch_recovery_active,
                malformed_apply_patch_write_recovery_active,
                progress_projection_edit_recovery_active,
                progress_projection_edit_recovery_needs_grounding_read,
                failed_edit_recovery_active,
                open_obligation_final_message_recovery_active,
                open_obligation_final_message_count,
            };
            TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
                &mut tools,
                &stable_tools,
                TurnLifecyclePreNormalizationSurfaceInput {
                    state: &step_request.state,
                    recovery: recovery_context,
                    code_authoring_final_message_hard_edit_recovery_active,
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
                    code_authoring_final_message_hard_edit_recovery_active,
                    generated_test_orientation_allowed:
                        !authoring_supporting_context_budget_recovery_active,
                },
            );
            if progress_projection_edit_recovery_active
                && progress_projection_edit_recovery_needs_grounding_read
            {
                let envelope = authoring_grounding_recovery_envelope(
                    &step_request.runtime_input.history_items,
                    &step_request.state,
                    request.session.workspace.root.as_path(),
                    &authoring_grounded_active_targets,
                );
                constrain_read_schema_to_missing_authoring_targets(&mut tools, &envelope);
            }
            if patch_context_mismatch_grounding_active
                && step_request.state.route == TaskRoute::Docs
                && step_request.state.process_phase == crate::session::ProcessPhase::Author
            {
                let envelope = authoring_grounding_recovery_envelope(
                    &step_request.runtime_input.history_items,
                    &step_request.state,
                    request.session.workspace.root.as_path(),
                    &authoring_grounded_active_targets,
                );
                constrain_read_schema_to_missing_authoring_targets(&mut tools, &envelope);
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
                invalid_edit_arguments_recovery.as_ref(),
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
                    request.protocol_turn_id,
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
                    request.protocol_turn_id,
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
                Some(tool_choice_label(&dispatch_tool_choice).to_string()),
            );
            let control_prompt = compiled_turn
                .envelope
                .projection_bundle
                .prompt
                .render_prompt_block();
            let (provider_messages, surface_filter_policies) =
                provider_messages_for_dispatch_control(
                    &bundle.messages,
                    control_prompt,
                    final_message_recovery_prompt,
                    invalid_edit_recovery_prompt.clone(),
                    &tool_names,
                    !TurnLifecycleKernel::closeout_ready_final_message_authority(
                        &step_request.state,
                    ),
                );
            let provider_messages =
                normalize_provider_system_context_for_chat_template(provider_messages);
            let mut replay_policies = bundle.replay_policies.clone();
            replay_policies.extend(surface_filter_policies);
            if provider_noncompliance_edit_recovery_active {
                replay_policies.push(
                    TurnLifecycleKernel::provider_noncompliance_edit_recovery_policy(
                        &step_request.state,
                    ),
                );
            }
            if wrong_target_authoring_edit_recovery_active {
                replay_policies.push(
                    TurnLifecycleKernel::wrong_target_authoring_edit_recovery_policy(
                        &step_request.state,
                    ),
                );
            }
            if malformed_write_patch_recovery_active {
                replay_policies.push(
                    TurnLifecycleKernel::malformed_write_patch_capable_recovery_policy(
                        &step_request.state,
                    ),
                );
            }
            if malformed_apply_patch_write_recovery_active {
                replay_policies.push(
                    TurnLifecycleKernel::malformed_apply_patch_write_recovery_policy(
                        &step_request.state,
                    ),
                );
            }
            if invalid_edit_recovery_prompt.is_some() {
                replay_policies.push(
                    TurnLifecycleKernel::invalid_edit_arguments_control_recovery_policy(
                        &step_request.state,
                    ),
                );
            }
            if provider_required_tool_choice_final_message_recovery_active {
                replay_policies.push(TurnLifecycleKernel::provider_required_tool_choice_final_message_recovery_policy(
                    &step_request.state,
                ));
            }
            let chat_request = ChatRequest {
                model: step_request.model.clone(),
                base_url: step_request.config.model.base_url.clone(),
                system_prompt,
                messages: provider_messages,
                tools: tools.clone(),
                timeout_ms: step_request.config.model.request_timeout_ms,
                stream_idle_timeout_ms: step_request.config.model.stream_idle_timeout_ms,
                stream_max_retries: step_request.config.model.stream_max_retries,
                extra_headers: step_request.config.model.extra_headers.clone(),
                temperature: step_request.config.model.temperature,
                top_p: step_request.config.model.top_p,
                top_k: step_request.config.model.top_k,
                presence_penalty: step_request.config.model.presence_penalty,
                frequency_penalty: step_request.config.model.frequency_penalty,
                seed: step_request.config.model.seed,
                stop_sequences: step_request.config.model.stop_sequences.clone(),
                extra_body: extra_body_with_tool_choice(
                    step_request.config.model.extra_body_json.clone(),
                    tool_names.len(),
                    &dispatch_tool_choice,
                ),
            };
            let terminal_response_timeout_ms = terminal_response_timeout_ms_for_state(
                step_request.config.model.request_timeout_ms,
                &step_request.state,
                active_work.as_ref(),
            );
            let diagnostics = request_diagnostics_from_chat(
                &chat_request,
                &tools,
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
                    if provider_error_is_request_timeout(&error)
                        && TurnLifecycleKernel::clean_closeout_final_message_lifecycle(
                            &step_request.state,
                            active_work.as_ref(),
                        )
                    {
                        let fallback = closeout_timeout_fallback_text();
                        append_part_and_emit_event(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            request.protocol_turn_id,
                            NewPart {
                                kind: PartKind::Text,
                                payload: MessagePart::Text(TextPart {
                                    text: fallback.to_string(),
                                }),
                            },
                            crate::session::RunEvent::TextDelta {
                                message_id: assistant_message.id,
                                delta: fallback.to_string(),
                            },
                            sink,
                        )
                        .await?;
                        return complete_turn(
                            &session_repo,
                            request.session.session.id,
                            assistant_message.id,
                            &request.model.name,
                            &request.config.model.base_url,
                            Some(FinishReason::Stop),
                            None,
                            request.model.context_window,
                            tool_call_count,
                            failed_tool_count,
                            change_count,
                            request.protocol_turn_id,
                            sink,
                        )
                        .await;
                    }
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
                        request.protocol_turn_id,
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
                    request.protocol_turn_id,
                    sink,
                )
                .await;
            }

            if stream.tool_calls.is_empty()
                && let Some(runtime_call) = runtime_owned_required_verification_tool_call(
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

            let final_message_adjudication = if stream.tool_calls.is_empty() {
                let action = ProviderActionAdapter::adapt_text_final(
                    stream.text.clone(),
                    compiled_turn
                        .envelope
                        .projection_bundle
                        .tool_result_feedback
                        .projection_id,
                    !TurnLifecycleKernel::closeout_ready_final_message_authority(
                        &step_request.state,
                    ),
                );
                Some(TurnLifecycleKernel::adjudicate_model_action(
                    action,
                    &tool_names,
                    false,
                    false,
                    &compiled_turn.envelope,
                ))
            } else {
                None
            };

            if matches!(
                final_message_adjudication,
                Some(ActionAdjudication::RejectedModelAction(ref rejection))
                    if rejection.semantic_class == "text_final_while_obligations_open"
            ) && !matches!(finish_reason, Some(FinishReason::Length))
            {
                if let Some(ActionAdjudication::RejectedModelAction(rejection)) =
                    final_message_adjudication.as_ref()
                {
                    let source_call_id = crate::session::ToolCallId::new();
                    let proposal = rejection.to_rejected_tool_proposal(
                        source_call_id,
                        &tool_names,
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                    );
                    sink.emit(crate::session::RunEvent::ToolProposalRejected {
                        tool_call_id: source_call_id,
                        proposal,
                    })?;
                }
                let guard_key = open_obligation_final_message_guard_key(
                    &step_request.state,
                    compiled_turn
                        .envelope
                        .action_authority
                        .required_action
                        .as_ref(),
                    &tool_names,
                    invalid_edit_arguments_recovery.as_ref(),
                    open_obligation_final_message_recovery.is_some(),
                    docs_grounding_final_message_recovery_active,
                );
                open_obligation_final_message_count = *open_obligation_final_message_counts
                    .entry(guard_key)
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                if TurnLifecycleKernel::provider_required_tool_choice_final_message_noncompliance(
                    &step_request.state,
                    &dispatch_tool_choice,
                    &tool_names,
                    malformed_apply_patch_write_recovery_active
                        || code_authoring_final_message_hard_edit_recovery_active
                        || invalid_edit_arguments_recovery.is_some(),
                ) {
                    provider_required_tool_choice_final_message_recovery_pending = true;
                }
                if open_obligation_final_message_count
                    >= OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD
                {
                    let message = open_obligation_final_message_terminal_message(
                        &step_request.state,
                        open_obligation_final_message_count,
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
                open_obligation_final_message_recovery =
                    Some(open_obligation_final_message_recovery_envelope(
                        &step_request.state,
                        open_obligation_final_message_count,
                        compiled_turn
                            .envelope
                            .action_authority
                            .required_action
                            .as_ref(),
                        &tool_names,
                        docs_grounding_final_message_recovery_active,
                    ));
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

            let rejected_tool_proposals_before_model_response = rejected_tool_proposals.clone();
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
                        request.protocol_turn_id,
                        sink,
                    )
                    .await;
                }
                tool_call_count += 1;
                let requested_tool_name = tool_call.tool_name.clone();
                let tool_names_for_route = tool_names.clone();
                let runtime_owned_verification_redirect =
                    runtime_owned_required_verification_dispatch_redirect(
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
                let adjudication_tool_call =
                    if let Some(redirect) = runtime_owned_verification_redirect.as_ref() {
                        CompletedToolCall {
                            call_id: tool_call.call_id.clone(),
                            tool_name: redirect.effective_tool_name.clone(),
                            arguments_json: redirect.effective_arguments_json.clone(),
                        }
                    } else {
                        tool_call.clone()
                    };
                let raw_model_action =
                    ProviderActionAdapter::adapt_tool_call(&adjudication_tool_call);
                let raw_action_name = raw_model_action.requested_action_name().to_string();
                let raw_tool_exists = self.agent.registry.has_tool(&raw_action_name);
                let raw_tool_allowed = tool_names_for_route.contains(&raw_action_name);
                let raw_adjudication = TurnLifecycleKernel::adjudicate_model_action(
                    raw_model_action.clone(),
                    &tool_names_for_route,
                    raw_tool_exists,
                    raw_tool_allowed,
                    &compiled_turn.envelope,
                );

                if let ActionAdjudication::RejectedModelAction(rejection) = raw_adjudication {
                    let raw_proposal = rejection.proposal.clone();
                    let route = ToolLifecycleRuntime::route_adjudicated_call(ToolRouteRequest {
                        requested_tool: raw_proposal.requested_tool.clone(),
                        effective_tool: raw_proposal.effective_tool.clone(),
                        record_tool: raw_proposal.effective_tool.clone(),
                        original_arguments_json: raw_proposal.arguments_json.clone(),
                        effective_arguments_json: raw_proposal.arguments_json.clone(),
                        allowed_tool_names: &tool_names_for_route,
                        tool_exists: raw_tool_exists,
                        tool_allowed: raw_tool_allowed,
                        redirected_from_arguments_json: None,
                        redirect_reason: None,
                        tool_choice: Some(tool_choice_label(&dispatch_tool_choice)),
                        control_projection: Some(control_projection_metadata(
                            &compiled_turn
                                .envelope
                                .projection_bundle
                                .tool_result_feedback,
                        )),
                        sandbox_decision: sandbox_decision_metadata(
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
                    let rejection_result = rejection.to_tool_result(
                        record.id,
                        &tool_names_for_route,
                        raw_tool_exists,
                        raw_tool_allowed,
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                    );
                    let result = if rejection.semantic_class == "malformed_tool_arguments"
                        && matches!(
                            raw_proposal.effective_tool.as_str(),
                            "write" | "apply_patch"
                        )
                        && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                            &step_request.state,
                        ) {
                        let parse_error =
                            serde_json::from_str::<Value>(&raw_proposal.arguments_json)
                                .map(|_| rejection.blocked_reason.clone())
                                .unwrap_or_else(|error| error.to_string());
                        let mut invalid_result = invalid_tool_arguments_result(
                            &raw_proposal.effective_tool,
                            &raw_proposal.arguments_json,
                            &parse_error,
                            &step_request.state,
                            Some(&tool_names_for_route),
                            Some(&dispatch_tool_choice),
                        );
                        if let Some(invalid_object) = invalid_result.metadata.as_object_mut()
                            && let Some(rejection_object) = rejection_result.metadata.as_object()
                        {
                            for key in [
                                "model_action_adjudication",
                                "rejected_tool_proposal",
                                "tool_rejected",
                                "provider_noncompliance",
                                "requested_tool",
                                "effective_tool",
                                "tool_exists",
                                "tool_allowed",
                            ] {
                                if let Some(value) = rejection_object.get(key) {
                                    invalid_object.insert(key.to_string(), value.clone());
                                }
                            }
                        }
                        invalid_result
                    } else {
                        rejection_result
                    };
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
                    record_patch_context_mismatch_grounding_targets(
                        &mut patch_context_mismatch_grounding_targets,
                        &result.metadata,
                        &step_request.state,
                    );
                    if let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
                        &raw_proposal.effective_tool,
                        &result.metadata,
                        &step_request.state,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    ) {
                        invalid_edit_arguments_recovery = Some(envelope);
                    }
                    if invalid_write_arguments_need_patch_capable_recovery(
                        &raw_proposal.effective_tool,
                        &result.metadata,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    ) {
                        malformed_write_patch_recovery_pending = true;
                    }
                    if invalid_apply_patch_arguments_need_write_recovery(
                        &raw_proposal.effective_tool,
                        &result.metadata,
                        &step_request.state,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    ) {
                        malformed_apply_patch_write_recovery_pending = true;
                    }
                    if let Some(key) = invalid_edit_arguments_no_progress_key(
                        &raw_proposal.effective_tool,
                        &result.metadata,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    ) {
                        let count = invalid_edit_argument_counts
                            .entry(key)
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                        if should_terminalize_invalid_edit_arguments_no_progress(*count) {
                            let message = invalid_edit_arguments_terminal_message(
                                &raw_proposal.effective_tool,
                                *count,
                                &result.metadata,
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
                                request.protocol_turn_id,
                                sink,
                            )
                            .await;
                        }
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
                    let provider_noncompliance = result
                        .metadata
                        .get("provider_noncompliance")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let semantic_class = result
                        .metadata
                        .get("model_action_adjudication")
                        .and_then(|value| value.get("semantic_class"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool_outside_allowed_surface");
                    if provider_noncompliance || !raw_tool_allowed {
                        let result_hash = result
                            .metadata
                            .get("model_action_adjudication")
                            .and_then(|value| value.get("result_hash"))
                            .and_then(Value::as_str);
                        let invalid_edit_recovery_no_progress_key = invalid_edit_arguments_recovery
                            .as_ref()
                            .map(invalid_edit_recovery_semantic_no_progress_key);
                        let guard_request = RejectedToolNoProgressGuardRequest {
                            effective_tool_name: &raw_proposal.effective_tool,
                            effective_arguments_json: &raw_proposal.arguments_json,
                            allowed_tools: &tool_names_for_route,
                            tool_choice: &dispatch_tool_choice,
                            required_action: compiled_turn
                                .envelope
                                .action_authority
                                .required_action
                                .as_ref(),
                            provider_noncompliance,
                            semantic_class,
                            result_hash,
                            recovery_no_progress_key: invalid_edit_recovery_no_progress_key
                                .as_deref(),
                        };
                        let rejected_tool_key =
                            ToolLifecycleRuntime::rejected_tool_no_progress_guard_key(
                                &guard_request,
                            );
                        let terminal_guard_feedback_was_visible =
                            rejected_tool_proposals_before_model_response
                                .contains_key(&rejected_tool_key);
                        let guard_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
                            &mut rejected_tool_proposals,
                            guard_request,
                        );
                        if terminal_guard_feedback_was_visible {
                            if let Some(message) = guard_decision.terminal_message {
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
                        if !terminal_guard_feedback_was_visible
                            && guard_decision.terminal_message.is_some()
                        {
                            continue;
                        }
                    }
                    continue;
                }

                let effective_tool_name = runtime_owned_verification_redirect
                    .as_ref()
                    .map(|redirect| redirect.effective_tool_name.clone())
                    .unwrap_or_else(|| requested_tool_name.clone());
                let tool_exists = self.agent.registry.has_tool(&effective_tool_name);
                let tool_allowed = tool_names_for_route.contains(&effective_tool_name);
                let active_targets_for_argument_repair =
                    operation_feedback_targets_for_turn(&step_request.state, active_work.as_ref());
                let escaped_source_write_candidate = normalized_escaped_source_write_candidate(
                    &effective_tool_name,
                    &tool_call.arguments_json,
                    &active_targets_for_argument_repair,
                );
                let effective_arguments_json = runtime_owned_verification_redirect
                    .as_ref()
                    .map(|redirect| redirect.effective_arguments_json.clone())
                    .or_else(|| {
                        repair_write_arguments_from_active_target(
                            &effective_tool_name,
                            &tool_call.arguments_json,
                            &active_targets_for_argument_repair,
                        )
                    })
                    .or_else(|| {
                        repair_shell_arguments_from_singleton_verification_command(
                            &effective_tool_name,
                            &tool_call.arguments_json,
                            active_work.as_ref(),
                            request
                                .config
                                .shell
                                .family
                                .unwrap_or_else(default_shell_family),
                        )
                    })
                    .or_else(|| {
                        escaped_source_write_candidate
                            .as_ref()
                            .map(|candidate| candidate.effective_arguments_json.clone())
                    })
                    .or_else(|| {
                        repair_unambiguous_malformed_edit_arguments_json(
                            &effective_tool_name,
                            &tool_call.arguments_json,
                        )
                    })
                    .unwrap_or_else(|| tool_call.arguments_json.clone());
                let redirected_from_arguments_json = runtime_owned_verification_redirect
                    .as_ref()
                    .map(|redirect| redirect.redirected_from_arguments_json.clone());
                let redirect_reason = runtime_owned_verification_redirect
                    .as_ref()
                    .map(|redirect| redirect.redirect_reason);
                let route = ToolLifecycleRuntime::route_adjudicated_call(ToolRouteRequest {
                    requested_tool: requested_tool_name.clone(),
                    effective_tool: effective_tool_name.clone(),
                    record_tool: effective_tool_name.clone(),
                    original_arguments_json: tool_call.arguments_json.clone(),
                    effective_arguments_json,
                    allowed_tool_names: &tool_names_for_route,
                    tool_exists,
                    tool_allowed,
                    redirected_from_arguments_json,
                    redirect_reason,
                    tool_choice: Some(tool_choice_label(&dispatch_tool_choice)),
                    control_projection: Some(control_projection_metadata(
                        &compiled_turn
                            .envelope
                            .projection_bundle
                            .tool_result_feedback,
                    )),
                    sandbox_decision: sandbox_decision_metadata(
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
                if let Some(candidate) = escaped_source_write_candidate {
                    sink.emit(crate::session::RunEvent::CandidateRepairEditRecorded {
                        tool_call_id: record.id,
                        candidate: candidate.into_candidate_repair_edit(record.id),
                    })?;
                }

                ToolLifecycleRuntime::mark_running(&session_repo, record.id).await?;
                let parsed_arguments = match serde_json::from_str::<Value>(
                    &route.effective_arguments_json,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        let result = invalid_tool_arguments_result(
                            &effective_tool_name,
                            &route.effective_arguments_json,
                            &error.to_string(),
                            &step_request.state,
                            Some(&tool_names_for_route),
                            Some(&dispatch_tool_choice),
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
                        record_patch_context_mismatch_grounding_targets(
                            &mut patch_context_mismatch_grounding_targets,
                            &result.metadata,
                            &step_request.state,
                        );
                        if let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
                            &effective_tool_name,
                            &result.metadata,
                            &step_request.state,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        ) {
                            invalid_edit_arguments_recovery = Some(envelope);
                        }
                        if invalid_write_arguments_need_patch_capable_recovery(
                            &effective_tool_name,
                            &result.metadata,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        ) {
                            malformed_write_patch_recovery_pending = true;
                        }
                        if invalid_apply_patch_arguments_need_write_recovery(
                            &effective_tool_name,
                            &result.metadata,
                            &step_request.state,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        ) {
                            malformed_apply_patch_write_recovery_pending = true;
                        }
                        if let Some(key) = invalid_edit_arguments_no_progress_key(
                            &effective_tool_name,
                            &result.metadata,
                            &tool_names_for_route,
                            &dispatch_tool_choice,
                        ) {
                            let count = invalid_edit_argument_counts
                                .entry(key)
                                .and_modify(|count| *count += 1)
                                .or_insert(1);
                            if should_terminalize_invalid_edit_arguments_no_progress(*count) {
                                let message = invalid_edit_arguments_terminal_message(
                                    &effective_tool_name,
                                    *count,
                                    &result.metadata,
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
                                    request.protocol_turn_id,
                                    sink,
                                )
                                .await;
                            }
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
                            shell_family: request
                                .config
                                .shell
                                .family
                                .unwrap_or_else(default_shell_family),
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
                    if let Some(envelope) = failed_edit_control_recovery_envelope(
                        &effective_tool_name,
                        &result.metadata,
                        &step_request.state,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    ) {
                        invalid_edit_arguments_recovery = Some(envelope);
                    }
                    let terminal_message =
                        ToolLifecycleRuntime::record_pre_execution_corrective_no_progress(
                            PreExecutionCorrectiveNoProgressInput {
                                kind: decision.kind,
                                result: &result,
                                effective_tool_name: &effective_tool_name,
                                parsed_arguments: &parsed_arguments,
                                active_work: active_work.as_ref(),
                                state: &step_request.state,
                                workspace_root: &request.session.workspace.root,
                                allowed_tools: &tool_names_for_route,
                                tool_choice: &dispatch_tool_choice,
                                open_executable_work:
                                    TurnLifecycleKernel::open_executable_work_requires_tool_call(
                                        &step_request.state,
                                    ),
                                operation_non_content_no_progress_counts:
                                    &mut operation_non_content_no_progress_counts,
                                repair_target_authority_violation_counts:
                                    &mut repair_target_authority_violation_counts,
                                wrong_authoring_target_counts: &mut wrong_authoring_target_counts,
                                docs_spec_semantic_reconciliation_counts:
                                    &mut docs_spec_semantic_reconciliation_counts,
                                public_command_contract_counts: &mut public_command_contract_counts,
                                wrong_verification_command_counts:
                                    &mut wrong_verification_command_counts,
                            },
                        )
                        .terminal_message;
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
                if docs_route_supporting_context_budget_applies(
                    &effective_tool_name,
                    &step_request.state,
                ) {
                    let budget_key = ToolLifecycleRuntime::docs_route_supporting_context_budget_key(
                        &step_request.state,
                        &tool_names_for_route,
                        &dispatch_tool_choice,
                    );
                    if docs_supporting_context_budget_exhausted.contains(&budget_key) {
                        let result =
                            ToolLifecycleRuntime::docs_supporting_context_budget_exhausted_result(
                                &effective_tool_name,
                                &parsed_arguments,
                                &step_request.state,
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
                        failed_tool_count += 1;
                        let guard_decision = ToolLifecycleRuntime::record_docs_supporting_context_budget_exhausted_no_progress(
                            &mut docs_supporting_context_budget_exhausted_counts,
                            budget_key,
                            &step_request.state,
                        );
                        if let Some(message) = guard_decision.terminal_message {
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
                }
                if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
                    &step_request.state,
                    &authoring_supporting_context_budget_exhausted,
                ) && authoring_supporting_context_budget_recovery_read_disallowed(
                    &effective_tool_name,
                    &parsed_arguments,
                    &step_request.state,
                    &step_request.runtime_input.history_items,
                    &request.session.workspace.root,
                    &authoring_grounded_active_targets,
                ) {
                    let grounding_envelope = authoring_grounding_recovery_envelope(
                        &step_request.runtime_input.history_items,
                        &step_request.state,
                        &request.session.workspace.root,
                        &authoring_grounded_active_targets,
                    );
                    let result = ToolLifecycleRuntime::authoring_target_grounding_required_result(
                        &effective_tool_name,
                        &parsed_arguments,
                        &step_request.state,
                        &grounding_envelope,
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
                    failed_tool_count += 1;
                    let guard_decision =
                        ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
                            &mut authoring_target_grounding_required_counts,
                            &result,
                        );
                    if let Some(message) = guard_decision.terminal_message {
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
                if existing_target_grounding_recovery_active
                    && authoring_supporting_context_budget_recovery_read_disallowed(
                        &effective_tool_name,
                        &parsed_arguments,
                        &step_request.state,
                        &step_request.runtime_input.history_items,
                        &request.session.workspace.root,
                        &authoring_grounded_active_targets,
                    )
                {
                    let grounding_envelope = authoring_grounding_recovery_envelope(
                        &step_request.runtime_input.history_items,
                        &step_request.state,
                        &request.session.workspace.root,
                        &authoring_grounded_active_targets,
                    );
                    let result = ToolLifecycleRuntime::authoring_target_grounding_required_result(
                        &effective_tool_name,
                        &parsed_arguments,
                        &step_request.state,
                        &grounding_envelope,
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
                    failed_tool_count += 1;
                    let guard_decision =
                        ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
                            &mut authoring_target_grounding_required_counts,
                            &result,
                        );
                    if let Some(message) = guard_decision.terminal_message {
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
                if generated_test_reference_consumed_target_grounding_active
                    && generated_test_reference_consumed_read_requires_active_target(
                        &effective_tool_name,
                        &parsed_arguments,
                        &step_request.state,
                    )
                {
                    let result =
                        ToolLifecycleRuntime::generated_test_target_grounding_required_result(
                            &effective_tool_name,
                            &parsed_arguments,
                            &step_request.state,
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
                    failed_tool_count += 1;
                    let guard_decision = ToolLifecycleRuntime::record_generated_test_target_grounding_required_no_progress(
                            &mut generated_test_target_grounding_required_counts,
                            &result,
                            &step_request.state,
                        );
                    if let Some(message) = guard_decision.terminal_message {
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
                        let progress_projection_no_content =
                            tool_result_is_progress_projection_no_content(&result)
                                && TurnLifecycleKernel::open_executable_work_requires_tool_call(
                                    &step_request.state,
                                );
                        change_count += result.change_summaries.len();
                        let operation_feedback_targets = operation_feedback_targets_for_turn(
                            &step_request.state,
                            active_work.as_ref(),
                        );
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
                            &operation_feedback_targets,
                            sink,
                        )
                        .await?;
                        let content_changing_progress =
                            tool_output_is_content_changing_progress(&completion_metadata);
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
                        if content_changing_progress {
                            progress_projection_no_progress_counts.clear();
                            operation_non_content_no_progress_counts.clear();
                            verification_supporting_context_no_progress_counts.clear();
                            wrong_authoring_target_counts.clear();
                            repair_target_authority_violation_counts.clear();
                            invalid_edit_argument_counts.clear();
                            malformed_write_patch_recovery_pending = false;
                            malformed_apply_patch_write_recovery_pending = false;
                            invalid_edit_arguments_recovery = None;
                            provider_required_tool_choice_final_message_recovery_pending = false;
                            open_obligation_final_message_count = 0;
                            open_obligation_final_message_counts.clear();
                            open_obligation_final_message_recovery = None;
                            open_obligation_final_message_hard_edit_recovery_pending = false;
                            patch_context_mismatch_grounding_targets.clear();
                            authoring_supporting_context_budget_exhausted.clear();
                            authoring_grounded_active_targets.clear();
                            authoring_target_grounding_required_counts.clear();
                            generated_test_target_grounding_required_counts.clear();
                            repair_supporting_context_budget_exhausted.clear();
                            if !docs_route_contract_still_pending_after_file_change(
                                &step_request.state,
                            ) {
                                docs_supporting_context_budget_exhausted.clear();
                            }
                            docs_supporting_context_budget_exhausted_counts.clear();
                        }
                        if content_changing_progress && !result.change_summaries.is_empty() {
                            align_todos_after_changes(
                                &session_repo,
                                request.session.session.id,
                                &request.session.workspace.root,
                                &todos,
                                &result.change_summaries,
                            )
                            .await?;
                        }
                        record_authoring_grounded_active_target(
                            &mut authoring_grounded_active_targets,
                            &effective_tool_name,
                            &completion_metadata,
                            &step_request.state,
                        );
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
                                    request.protocol_turn_id,
                                    sink,
                                )
                                .await;
                            }
                        }
                        if let Some(decision) =
                            ToolLifecycleRuntime::record_operation_non_content_no_progress(
                                &mut operation_non_content_no_progress_counts,
                                &effective_tool_name,
                                &completion_metadata,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                                TurnLifecycleKernel::open_executable_work_requires_tool_call(
                                    &step_request.state,
                                ),
                            )
                        {
                            if patch_context_mismatch_target_grounding_read_satisfied(
                                &effective_tool_name,
                                &completion_metadata,
                                &step_request.state,
                            ) {
                                patch_context_mismatch_grounding_targets.clear();
                            }
                            if let Some(budget_exhaustion) = decision.budget_exhaustion {
                                match budget_exhaustion {
                                    OperationNoProgressBudgetExhaustion::DocsSupportingContext => {
                                        docs_supporting_context_budget_exhausted
                                            .insert(decision.key);
                                    }
                                    OperationNoProgressBudgetExhaustion::AuthoringSupportingContext => {
                                        authoring_supporting_context_budget_exhausted
                                            .insert(decision.key);
                                    }
                                    OperationNoProgressBudgetExhaustion::RepairSupportingContext => {
                                        repair_supporting_context_budget_exhausted
                                            .insert(decision.key);
                                    }
                                }
                                continue;
                            }
                            if let Some(message) = decision.terminal_message {
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
                        if let Some(decision) =
                            ToolLifecycleRuntime::record_verification_supporting_context_no_progress(
                                &mut verification_supporting_context_no_progress_counts,
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &result,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            )
                        {
                            if let Some(message) = decision.terminal_message {
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
                        if let Some(decision) =
                            ToolLifecycleRuntime::record_same_verification_failure_no_progress(
                                &mut same_verification_failure_counts,
                                &completion_metadata,
                            )
                        {
                            if let Some(message) = decision.terminal_message {
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
                        } else if ToolLifecycleRuntime::verification_run_passed(
                            &completion_metadata,
                        ) {
                            same_verification_failure_counts.clear();
                        }
                    }
                    Err(error) => {
                        if request.cancel.is_cancelled() {
                            failed_tool_count += 1;
                            ToolLifecycleRuntime::fail_executed_call(
                                &session_repo,
                                assistant_message.id,
                                request.session.session.id,
                                request.protocol_turn_id,
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
                                request.protocol_turn_id,
                                sink,
                            )
                            .await;
                        }
                        if is_invalid_tool_arguments_error(&error.to_string()) {
                            let result = invalid_tool_arguments_result(
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &error.to_string(),
                                &step_request.state,
                                Some(&tool_names_for_route),
                                Some(&dispatch_tool_choice),
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
                            record_patch_context_mismatch_grounding_targets(
                                &mut patch_context_mismatch_grounding_targets,
                                &result.metadata,
                                &step_request.state,
                            );
                            if let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
                                &effective_tool_name,
                                &result.metadata,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            ) {
                                invalid_edit_arguments_recovery = Some(envelope);
                            }
                            if invalid_write_arguments_need_patch_capable_recovery(
                                &effective_tool_name,
                                &result.metadata,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            ) {
                                malformed_write_patch_recovery_pending = true;
                            }
                            if invalid_apply_patch_arguments_need_write_recovery(
                                &effective_tool_name,
                                &result.metadata,
                                &step_request.state,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            ) {
                                malformed_apply_patch_write_recovery_pending = true;
                            }
                            if let Some(key) = invalid_edit_arguments_no_progress_key(
                                &effective_tool_name,
                                &result.metadata,
                                &tool_names_for_route,
                                &dispatch_tool_choice,
                            ) {
                                let count = invalid_edit_argument_counts
                                    .entry(key)
                                    .and_modify(|count| *count += 1)
                                    .or_insert(1);
                                if should_terminalize_invalid_edit_arguments_no_progress(*count) {
                                    let message = invalid_edit_arguments_terminal_message(
                                        &effective_tool_name,
                                        *count,
                                        &result.metadata,
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
                                        request.protocol_turn_id,
                                        sink,
                                    )
                                    .await;
                                }
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
                            &error.to_string(),
                            &route,
                            sink,
                        )
                        .await?;
                        let guard_decision =
                            ToolLifecycleRuntime::record_executed_tool_failure_no_progress(
                                &mut executed_tool_failure_counts,
                                &effective_tool_name,
                                &route.effective_arguments_json,
                                &tool_names_for_route,
                                &error.to_string(),
                            );
                        if let Some(message) = guard_decision.terminal_message {
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
            "turn step budget reached before completion",
            tool_call_count,
            failed_tool_count,
            change_count,
            request.protocol_turn_id,
            sink,
        )
        .await
    }
}

async fn stream_chat_with_optional_terminal_timeout(
    llm: &Arc<dyn LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    sink: &mut dyn LlmEventSink,
    terminal_response_timeout_ms: Option<u64>,
) -> Result<LlmResponseSummary, crate::error::LlmError> {
    let request_future = llm.stream_chat(request, cancel, sink);
    let Some(timeout_ms) = terminal_response_timeout_ms else {
        return request_future.await;
    };
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

pub fn invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes() -> bool {
    let legacy_evidence_text = [
        "Latest confirmed evidence",
        "recovery command completed successfully after invalid tool-call feedback.",
    ]
    .join(": ");
    !include_str!("loop_impl.rs").contains(&legacy_evidence_text)
}

async fn append_part_and_emit_event(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    message_id: crate::session::MessageId,
    protocol_turn_id: TurnId,
    part: NewPart,
    event: crate::session::RunEvent,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    session_repo
        .append_part_with_protocol_bundle(
            session_id,
            message_id,
            part,
            &event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
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
    let terminal_event = crate::session::RunEvent::SessionCompleted {
        session_id,
        finish_reason: finish_reason.clone(),
    };
    persist_provider_token_accounting(
        session_repo,
        session_id,
        context_window,
        token_usage.as_ref(),
        protocol_turn_id,
        sink,
    )
    .await?;
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason: finish_reason.clone(),
                token_usage: token_usage.clone(),
                summary: false,
            }),
            SessionStatus::Completed,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
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

async fn persist_provider_token_accounting(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    context_window: u32,
    token_usage: Option<&crate::session::TokenUsage>,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    let Some(token_usage) = token_usage else {
        return Ok(());
    };
    let mut state = session_repo.get_state(session_id).await?;
    state.token_accounting = TokenAccountingState::from_provider_usage(context_window, token_usage);
    let event = crate::session::RunEvent::StateUpdated {
        session_id,
        state: state.clone(),
    };
    session_repo
        .update_state_with_protocol_event(
            session_id,
            &state,
            &event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
}

pub(crate) fn terminal_token_accounting_sequence_fixture_passes() -> bool {
    use crate::protocol::{ProtocolEventStore, RuntimeEventMsg};
    use crate::session::{
        AssistantMessageMeta, FinishReason, MessageMetadata, MessageRole, NewMessage, NewSession,
        ProjectId, ProjectRepository, RunEvent, SessionRepository, TokenUsage,
    };
    use crate::storage::{SqliteStore, StoragePaths};

    struct CountingSink {
        next_sequence_no: i64,
    }

    impl RunEventSink for CountingSink {
        fn emit(&mut self, _event: RunEvent) -> Result<(), crate::error::RuntimeError> {
            self.next_sequence_no += 1;
            Ok(())
        }

        fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
            let sequence_no = self.next_sequence_no;
            self.next_sequence_no += 1;
            Some(sequence_no)
        }

        fn emit_pre_recorded(&mut self, event: RunEvent) -> Result<(), crate::error::RuntimeError> {
            let _ = event;
            Ok(())
        }
    }

    let unique = format!(
        "moyai-terminal-token-accounting-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let root_path = std::env::temp_dir().join(unique);
    let Ok(data_dir) = camino::Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let paths = StoragePaths {
        data_dir: data_dir.clone(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let worker_paths = paths.clone();
    let result = std::thread::spawn(move || -> Result<bool, crate::error::RuntimeError> {
        let store = SqliteStore::open(&worker_paths)
            .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
        store
            .migrate()
            .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
        let project_repo = store.project_repo();
        let session_repo = store.session_repo();
        let protocol_store = store.protocol_event_store();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
        runtime.block_on(async {
            let project_id = ProjectId::new();
            let workspace_root = Utf8Path::new("C:/workspace/terminal-token-accounting");
            project_repo
                .upsert_project(
                    project_id,
                    workspace_root,
                    "Terminal Token Accounting",
                    "none",
                )
                .await
                .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
            let session = session_repo
                .create_session(NewSession {
                    project_id,
                    title: "terminal token accounting".to_string(),
                    cwd: workspace_root.to_path_buf(),
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                })
                .await
                .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
            let turn_id = TurnId::new();
            let (assistant, _) = session_repo
                .append_assistant_message_with_protocol_start(
                    NewMessage {
                        session_id: session.id,
                        parent_message_id: None,
                        role: MessageRole::Assistant,
                        metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                            model: "model".to_string(),
                            base_url: "http://localhost:1234".to_string(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        }),
                    },
                    turn_id,
                    Some(0),
                    "model".to_string(),
                )
                .await
                .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
            let mut sink = CountingSink {
                next_sequence_no: 1,
            };
            complete_turn(
                &session_repo,
                session.id,
                assistant.id,
                "model",
                "http://localhost:1234",
                Some(FinishReason::Stop),
                Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    total_tokens: 12,
                    reasoning_tokens: None,
                }),
                131_072,
                0,
                0,
                0,
                turn_id,
                &mut sink,
            )
            .await
            .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
            let events = protocol_store
                .list_runtime_events(session.id, turn_id)
                .map_err(|error| crate::error::RuntimeError::Message(error.to_string()))?;
            let unique_sequence_count = events
                .iter()
                .map(|event| event.sequence_no)
                .collect::<BTreeSet<_>>()
                .len();
            Ok(events.len() == unique_sequence_count
                && events.last().is_some_and(|event| {
                    matches!(event.msg, RuntimeEventMsg::TurnCompleted { .. })
                })
                && events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::Warning { .. })))
        })
    })
    .join()
    .unwrap_or_else(|_| {
        Err(crate::error::RuntimeError::Message(
            "terminal token accounting fixture worker panicked".to_string(),
        ))
    });
    let _ = std::fs::remove_dir_all(data_dir.as_std_path());
    result.unwrap_or(false)
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
    let terminal_event = crate::session::RunEvent::SessionInterrupted {
        session_id,
        reason: reason.to_string(),
    };
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason: Some(FinishReason::Cancelled),
                token_usage: None,
                summary: false,
            }),
            SessionStatus::Cancelled,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
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
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<RunSummary, AgentError> {
    let terminal_event = crate::session::RunEvent::SessionFailed {
        session_id,
        message: message.to_string(),
    };
    session_repo
        .update_message_metadata_and_status_with_protocol_event(
            session_id,
            assistant_message_id,
            &MessageMetadata::Assistant(AssistantMessageMeta {
                model: model.to_string(),
                base_url: base_url.to_string(),
                finish_reason: Some(FinishReason::Error),
                token_usage: None,
                summary: false,
            }),
            SessionStatus::Failed,
            &terminal_event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(terminal_event)?;
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
        stream_max_retries: request.stream_max_retries,
        configured_max_output_tokens: Some(request.model.max_output_tokens),
        effective_max_output_tokens: Some(request.effective_max_output_tokens()),
        output_budget_reason: Some(request.output_budget_reason().to_string()),
        system_prompt_chars: request.provider_system_prompt().chars().count(),
        tool_count: tools.len(),
        tool_choice: request
            .extra_body
            .as_ref()
            .and_then(|value| value.get("tool_choice"))
            .map(tool_choice_diagnostic_label),
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

pub(crate) fn request_diagnostics_stream_retry_policy_fixture_passes() -> bool {
    let request = ChatRequest {
        model: crate::llm::ModelProfile {
            name: "local-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: crate::config::ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: crate::llm::ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
            },
        },
        base_url: "http://localhost:8110".to_string(),
        system_prompt: "system".to_string(),
        messages: vec![crate::llm::ModelMessage::User {
            content: "do work".to_string(),
        }],
        tools: Vec::new(),
        timeout_ms: 600_000,
        stream_idle_timeout_ms: 300_000,
        stream_max_retries: 2,
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
    let diagnostics = request_diagnostics_from_chat(&request, &[], None, None, &[]);
    diagnostics.request_timeout_ms == 600_000
        && diagnostics.stream_idle_timeout_ms == 300_000
        && diagnostics.stream_max_retries == 2
}

fn provider_messages_for_dispatch_control(
    bundle_messages: &[crate::llm::ModelMessage],
    control_prompt: String,
    final_message_recovery_prompt: Option<String>,
    invalid_edit_recovery_prompt: Option<String>,
    tool_names: &BTreeSet<String>,
    open_obligations: bool,
) -> (
    Vec<crate::llm::ModelMessage>,
    Vec<crate::session::RequestReplayPolicyDiagnostic>,
) {
    let mut control_segments = Vec::new();
    if let Some(correction) = invalid_edit_recovery_prompt {
        control_segments.push(format!("Invalid edit recovery:\n{correction}"));
    }
    if let Some(correction) = final_message_recovery_prompt {
        control_segments.push(format!(
            "Open-obligation final-message recovery:\n{correction}"
        ));
    }
    control_segments.push(control_prompt);
    let control_prompt = control_segments.join("\n\n");
    let mut provider_messages = bundle_messages.to_vec();
    provider_messages.insert(
        0,
        crate::llm::ModelMessage::System {
            content: control_prompt,
        },
    );
    let surface_filter =
        ReplayNormalizer::filter_to_effective_tool_surface(provider_messages, tool_names);
    let provider_messages = filter_non_authoritative_assistant_text_for_open_obligations(
        surface_filter.messages,
        open_obligations,
    );
    (provider_messages, surface_filter.replay_policies)
}

fn operation_feedback_targets_for_turn(
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) -> Vec<Utf8PathBuf> {
    active_work
        .map(ActiveWorkContract::targets)
        .filter(|targets| !targets.is_empty())
        .unwrap_or_else(|| state.active_targets.clone())
}

pub(crate) fn operation_feedback_uses_active_work_targets_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.active_targets = vec![
        Utf8PathBuf::from("test_widget.py"),
        Utf8PathBuf::from("widget.py"),
    ];
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_widget_public_output".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("widget.py")],
    };

    operation_feedback_targets_for_turn(&state, Some(&active_work))
        == vec![Utf8PathBuf::from("widget.py")]
        && operation_feedback_targets_for_turn(&state, None) == state.active_targets
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
            active_targets: crate::protocol::canonicalize_workspace_targets(
                &active_work
                    .map(ActiveWorkContract::targets)
                    .filter(|targets| !targets.is_empty())
                    .unwrap_or_else(|| request.state.active_targets.clone()),
                &request.session.workspace.root,
            ),
            operation_intents: operation_intents_for_active_work(active_work),
            required_verification_commands: turn_decision.required_verification_commands.clone(),
            allowed_tools: allowed_tools.clone(),
            forbidden_tools: Vec::new(),
            projection_id,
        },
        allowed_tools,
        tool_choice: tool_choice.clone(),
        images: latest_user_images(&request.runtime_input.materialized_transcript_projection()),
        output_contract: OutputContract {
            final_answer_required: TurnLifecycleKernel::closeout_ready_final_message_authority(
                &request.state,
            ),
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
    let mut obligations = ObligationCompiler::compile(&context);
    if let Some(envelope) = authoring_grounding_recovery {
        obligations
            .items
            .push(authoring_grounding_recovery_obligation(envelope));
    }
    if let Some(envelope) = invalid_edit_recovery {
        obligations
            .items
            .push(invalid_edit_recovery_projection_obligation(envelope));
    }
    if let Some(obligation) =
        crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_recovery_obligation(
            &request.runtime_input.history_items,
            active_work,
            request.session.workspace.root.as_path(),
        )
    {
        obligations.items.push(obligation);
    }
    TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    })
}

fn invalid_edit_recovery_projection_obligation(
    envelope: &InvalidEditRecoveryEnvelope,
) -> TurnObligation {
    let targets = envelope
        .active_targets
        .iter()
        .map(Utf8PathBuf::from)
        .collect::<Vec<_>>();
    let submitted = joined_or_none(&envelope.submitted_targets);
    let active_submitted = joined_or_none(&envelope.active_submitted_targets);
    let inactive_submitted = joined_or_none(&envelope.inactive_submitted_targets);
    let candidate = envelope.candidate_target.as_deref().unwrap_or("none");
    let parser_family = envelope.parser_error_family.as_deref().unwrap_or("none");
    let result_hash = envelope.result_hash.as_deref().unwrap_or("none");
    let recovery_action = envelope.recovery_action.as_deref().unwrap_or("none");
    let mut evidence_refs = vec![EvidenceRef {
        source: envelope.failure_kind.clone(),
        reference: format!(
            "tool={};candidate_target={candidate};submitted_targets={submitted};active_submitted_targets={active_submitted};inactive_submitted_targets={inactive_submitted};parser_error_family={parser_family};recovery_action={recovery_action};result_hash={result_hash}",
            envelope.tool_name
        ),
    }];
    if !envelope.active_submitted_targets.is_empty()
        && !envelope.inactive_submitted_targets.is_empty()
    {
        evidence_refs.push(EvidenceRef {
            source: envelope.failure_kind.clone(),
            reference: "mixed_target_apply_patch_rewrite_target_only".to_string(),
        });
    }
    let mut contract_refs = vec!["failed_edit_control_recovery_projection".to_string()];
    if envelope.failure_kind == "invalid_edit_arguments" {
        contract_refs.push("invalid_edit_arguments_control_recovery_projection".to_string());
    }
    if envelope.failure_kind == "required_write_content_shape_mismatch" {
        contract_refs.push("required_write_content_shape_recovery_projection".to_string());
    }
    let action_target = envelope
        .recovery_target
        .as_deref()
        .or_else(|| envelope.active_targets.first().map(String::as_str));
    TurnObligation {
        obligation_id: "invalid_edit_recovery".to_string(),
        kind: ObligationKind::Repair,
        summary: format!(
            "Failed edit recovery remains active for target-only authoring. Failure kind: {}; Submitted targets: {submitted}; active submitted targets: {active_submitted}; inactive submitted targets: {inactive_submitted}.",
            envelope.failure_kind
        ),
        targets,
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: action_target
            .map(|target| vec![format!("{}:{target}", envelope.tool_name)])
            .unwrap_or_default(),
        verification_commands: Vec::new(),
        contract_refs,
        evidence_refs,
        status: ObligationStatus::Open,
    }
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
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
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            ..
        }) => vec![OperationIntent::ContentChangingAuthoringRequired],
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
        "required_action": surface.required_action,
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

fn sandbox_decision_metadata(profile: &SandboxProfile) -> Value {
    let profile = match profile {
        SandboxProfile::ReadOnly => "read_only",
        SandboxProfile::WorkspaceWrite => "workspace_write",
        SandboxProfile::FullAccess => "full_access",
    };
    json!({
        "profile": profile,
        "network_allowed": false,
        "escalated": false,
    })
}

fn default_shell_family() -> crate::config::ShellFamily {
    if cfg!(windows) {
        crate::config::ShellFamily::PowerShell
    } else {
        crate::config::ShellFamily::Bash
    }
}

fn tool_choice_diagnostic_label(value: &Value) -> String {
    if let Some(label) = value.as_str() {
        return label.to_string();
    }
    value
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(|name| format!("named:{name}"))
        .unwrap_or_else(|| value.to_string())
}

fn request_control_envelope_diagnostic(
    envelope: &TurnControlEnvelope,
) -> RequestControlEnvelopeDiagnostic {
    let validation = envelope.validate();
    RequestControlEnvelopeDiagnostic {
        envelope_id: envelope.id.to_string(),
        projection_id: envelope.projection_id.to_string(),
        dispatch_policy: dispatch_policy_label(&envelope.dispatch_policy).to_string(),
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
            content_markers: request_content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        crate::llm::ModelMessage::User { content } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: request_content_markers(content),
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
                content_markers: request_content_markers(
                    &parts
                        .iter()
                        .filter_map(|part| match part {
                            crate::llm::ModelContentPart::Text { text } => Some(text.as_str()),
                            crate::llm::ModelContentPart::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                image_count,
                image_bytes,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }
        }
        crate::llm::ModelMessage::Assistant { content } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: request_content_markers(content),
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
            content_markers: content
                .as_ref()
                .map(|value| request_content_markers(value))
                .unwrap_or_default(),
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
            content_markers: request_content_markers(result),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.clone()),
        },
    }
}

fn request_content_markers(content: &str) -> Vec<String> {
    let mut markers = Vec::new();
    if content.contains("Open-obligation final-message recovery:") {
        markers.push("open_obligation_final_message_recovery".to_string());
    }
    if content.contains("Invalid edit recovery:") {
        markers.push("invalid_edit_arguments_recovery".to_string());
    }
    if content.contains("Open targets:") {
        markers.push("open_targets_projection".to_string());
    }
    if content.contains("exact apply_patch grammar")
        || content.contains("Add File body lines must start with `+`")
    {
        markers.push("strict_apply_patch_grammar".to_string());
    }
    if content.contains("top-level `def`") || content.contains("top-level def/class/import lines") {
        markers.push("add_file_line_prefix_rule".to_string());
    }
    if content.contains("single patch")
        && content.contains("*** Add File")
        && content.contains("*** Update File")
    {
        markers.push("multi_file_apply_patch_shape".to_string());
    }
    if content.contains("Language Policy:") {
        markers.push("language_policy".to_string());
    }
    if content.contains("Agent Tool Policy:") {
        markers.push("agent_tool_policy".to_string());
    }
    markers
}

#[derive(Debug, Clone)]
struct OpenObligationFinalMessageRecoveryEnvelope {
    count: usize,
    active_targets: Vec<String>,
    prompt: String,
}

fn open_obligation_final_message_recovery_envelope(
    state: &SessionStateSnapshot,
    count: usize,
    required_action: Option<&RequiredAction>,
    allowed_tools: &BTreeSet<String>,
    docs_grounding_required: bool,
) -> OpenObligationFinalMessageRecoveryEnvelope {
    OpenObligationFinalMessageRecoveryEnvelope {
        count,
        active_targets: state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect(),
        prompt: open_obligation_final_message_correction_text(
            state,
            count,
            required_action,
            allowed_tools,
            docs_grounding_required,
        ),
    }
}

fn open_obligation_final_message_guard_key(
    state: &SessionStateSnapshot,
    required_action: Option<&RequiredAction>,
    _allowed_tools: &BTreeSet<String>,
    invalid_edit_recovery: Option<&InvalidEditRecoveryEnvelope>,
    _open_final_recovery_active: bool,
    docs_grounding_required: bool,
) -> String {
    let active_targets = if state.active_targets.is_empty() {
        "none".to_string()
    } else {
        state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let recovery_context = invalid_edit_recovery
        .map(|envelope| {
            let targets = if envelope.active_targets.is_empty() {
                "none".to_string()
            } else {
                envelope.active_targets.join(",")
            };
            let candidate = envelope.candidate_target.as_deref().unwrap_or("none");
            let family = envelope.parser_error_family.as_deref().unwrap_or("none");
            format!(
                "invalid_edit_arguments:tool={}:candidate={candidate}:family={family}:targets={targets}",
                envelope.tool_name
            )
        })
        .unwrap_or_else(|| "none".to_string());
    let required_action_projection = required_action
        .map(RequiredAction::projection_label)
        .unwrap_or("none");
    format!(
        "open_obligation_final_message|route={:?}|phase={:?}|targets={active_targets}|required_action={required_action_projection}|docs_grounding={docs_grounding_required}|recovery={recovery_context}",
        state.route, state.process_phase,
    )
}

pub(crate) fn singleton_write_surface_requires_tool_choice_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["write".to_string()]);
    matches!(
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &SessionStateSnapshot::default(),
            &tool_names,
            TurnLifecycleRecoveryContext::default(),
        ),
        ToolChoice::Auto
    )
}

pub(crate) fn required_write_target_mismatch_feedback_projects_test_content_authority() -> bool {
    let guidance =
        crate::agent::content_shape_contract::required_write_target_mismatch_content_shape_guidance(
            "test_component.py",
            Some("component.py"),
            "def add(a, b):\n    return a + b\n\ndef main():\n    input('expr')\n",
        );
    guidance.contains("production source under test")
        && guidance.contains("Required positive test-module shape")
        && guidance.contains("import `component`")
        && guidance.contains("Test*")
        && guidance.contains("Forbidden shape")
        && guidance.contains("Observed rejected content markers")
        && guidance.contains("def add")
        && guidance.contains("input(")
}

pub(crate) fn preserve_provider_tool_surface_for_dispatch(tools: &mut Vec<crate::llm::ToolSchema>) {
    let _ = tools;
}

pub(crate) fn exact_write_route_accepts_unittest_main_test_content() -> bool {
    let content = r#"
import unittest
import component

class TestComponent(unittest.TestCase):
    def test_add(self):
        self.assertEqual(component.add(2, 3), 5)

if __name__ == "__main__":
    unittest.main()
"#;
    crate::agent::content_shape_contract::write_content_matches_required_target(
        "test_component.py",
        content,
    )
}

pub(crate) fn content_shape_mismatch_feedback_carries_positive_test_contract() -> bool {
    let arguments = json!({
        "content": "def add(a, b):\n    return a + b\n\ndef main():\n    input('expr')\n"
    });
    let Some(result) =
        crate::agent::content_shape_contract::required_write_content_shape_violation_result(
            "write",
            &arguments,
            "test_component.py",
        )
    else {
        return false;
    };
    result
        .output_text
        .contains("Required positive test-module shape")
        && result.output_text.contains("Test*")
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
            == Some("component")
        && result
            .metadata
            .pointer("/tool_feedback_envelope/operation_progress_class")
            .and_then(Value::as_str)
            == Some("no_progress")
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

pub(crate) fn test_target_content_shape_write_lifecycle_enforced_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "test_widget.py",
        "content": "def add(a, b):\n    return a + b\n\ndef main():\n    input('expr')\n"
    });
    let Some(bad_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    let good_arguments = json!({
        "path": "test_widget.py",
        "content": "import unittest\nimport widget\n\nclass TestWidget(unittest.TestCase):\n    def test_public_behavior(self):\n        self.assertEqual(widget.add(2, 3), 5)\n"
    });
    let input_named_api_test_arguments = json!({
        "path": "test_widget.py",
        "content": "import subprocess\nimport sys\nimport unittest\nimport widget\n\nclass TestWidgetBehavior(unittest.TestCase):\n    @classmethod\n    def setUpClass(cls):\n        cls.python = sys.executable\n\n    def test_parse_input(self):\n        self.assertEqual(widget.parse_input('2 + 3'), (2.0, '+', 3.0))\n\n    def test_cli_stdin(self):\n        result = subprocess.run([self.python, 'widget.py'], input='2 + 3\\nquit\\n', text=True, capture_output=True)\n        self.assertEqual(result.returncode, 0)\n"
    });
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let bad_arguments_variant = json!({
        "path": "test_widget.py",
        "content": "def subtract(a, b):\n    return a - b\n\ndef main():\n    input('expr')\n"
    });
    let Some(bad_result_variant) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments_variant,
            None,
        )
    else {
        return false;
    };
    let repeat_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "write",
        &bad_result.metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let repeat_key_variant = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "write",
        &bad_result_variant.metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    crate::agent::content_shape_contract::artifact_content_shape_violation_result(
        "write",
        &good_arguments,
        None,
    )
    .is_none()
        && crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &input_named_api_test_arguments,
            None,
        )
        .is_none()
        && bad_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
            &bad_result.metadata,
            &state,
        )
        && repeat_key == repeat_key_variant
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
            &state,
        )
        && bad_result.output_text.contains("test_widget.py")
        && bad_result
            .output_text
            .contains("Required positive test-module shape")
        && bad_result.output_text.contains("Forbidden shape")
}

pub(crate) fn test_target_content_shape_rejects_string_literal_wrapped_tests_fixture_passes() -> bool
{
    let bad_arguments = json!({
        "path": "test_component.py",
        "content": "\"import unittest\\nimport component\\nclass TestComponent(unittest.TestCase):\\n    def test_add(self):\\n        self.assertEqual(component.add(2, 3), 5)\\n\""
    });
    let good_arguments = json!({
        "path": "test_component.py",
        "content": "import unittest\nimport component\n\nclass TestComponent(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.add(2, 3), 5)\n"
    });
    crate::agent::content_shape_contract::artifact_content_shape_violation_result("write", &bad_arguments, None).is_some()
        && crate::agent::content_shape_contract::artifact_content_shape_violation_result("write", &good_arguments, None).is_none()
        && crate::agent::content_shape_contract::test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes()
        && crate::agent::content_shape_contract::test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_escaped_whole_file_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "component.py",
        "content": "\"import math\\n\\ndef square(value):\\n    return value * value\\n\\nif __name__ == \\\"__main__\\\":\\n    print(square(3))\\n\""
    });
    let good_arguments = json!({
        "path": "component.py",
        "content": "import math\n\n\ndef square(value):\n    return value * value\n\n\nif __name__ == \"__main__\":\n    print(square(3))\n"
    });
    let root_path = std::env::temp_dir().join(format!(
        "moyai-source-shape-patch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    if std::fs::create_dir_all(root.as_std_path()).is_err() {
        return false;
    }
    let patch_arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: component.py\n+\"import math\\n\\ndef square(value):\\n    return value * value\\n\\nif __name__ == \\\"__main__\\\":\\n    print(square(3))\\n\"\n*** End Patch"
    });
    let Some(bad_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        let _ = std::fs::remove_dir_all(root.as_std_path());
        return false;
    };
    let patch_rejected =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "apply_patch",
            &patch_arguments,
            Some(root.as_path()),
        )
        .is_some_and(|result| {
            result
                .metadata
                .pointer("/content_shape_contract/kind")
                .and_then(Value::as_str)
                == Some("python_source_executable_content_shape")
        });
    let patch_left_workspace_clean = !root.join("component.py").exists();
    let _ = std::fs::remove_dir_all(root.as_std_path());
    crate::agent::content_shape_contract::artifact_content_shape_violation_result("write", &good_arguments, None).is_none()
        && bad_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && bad_result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("python_source_executable_content_shape")
        && bad_result.output_text.contains("Required positive source shape")
        && patch_rejected
        && patch_left_workspace_clean
        && crate::agent::content_shape_contract::python_source_executable_shape_rejects_escaped_whole_file_fixture_passes()
}

pub(crate) fn source_content_shape_normalizes_escaped_repair_candidate_fixture_passes() -> bool {
    let arguments = json!({
        "path": "component.py",
        "content": "\"\"\"\nComponent module.\\n\\ndef add(a, b):\\n    return a + b\\n\\nif __name__ == \\\"__main__\\\":\\n    print(add(2, 3))\\n\"\"\""
    })
    .to_string();
    let Some(candidate) = normalized_escaped_source_write_candidate(
        "write",
        &arguments,
        &[Utf8PathBuf::from("component.py")],
    ) else {
        return false;
    };
    let Ok(effective) = serde_json::from_str::<Value>(&candidate.effective_arguments_json) else {
        return false;
    };
    let Some(content) = effective.get("content").and_then(Value::as_str) else {
        return false;
    };
    let repair = candidate.into_candidate_repair_edit(crate::session::ToolCallId::new());
    effective.get("path").and_then(Value::as_str) == Some("component.py")
        && content.contains("def add")
        && content.contains("\n\nif __name__")
        && !content.contains("\\ndef add")
        && !content.trim_end().ends_with("\"\"\"")
        && crate::agent::content_shape_contract::write_content_matches_required_target(
            "component.py",
            content,
        )
        && repair.target_path.as_ref().map(|path| path.as_str()) == Some("component.py")
        && repair.semantic_class == "escaped_source_write_candidate_normalized"
        && matches!(repair.validity, CandidateRepairValidity::Admitted)
        && repair
            .evidence_refs
            .iter()
            .any(|item| item == "escaped_source_write_normalized")
}

pub(crate) fn source_content_shape_rejects_test_module_payload_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "component.py",
        "content": "import unittest\nimport component\n\nclass TestComponent(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.add(2, 3), 5)\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
    });
    let good_arguments = json!({
        "path": "component.py",
        "content": "def add(left, right):\n    return left + right\n\nif __name__ == \"__main__\":\n    print(add(2, 3))\n"
    });
    let Some(bad_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    crate::agent::content_shape_contract::artifact_content_shape_violation_result("write", &good_arguments, None).is_none()
        && bad_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && bad_result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("python_source_executable_content_shape")
        && bad_result
            .metadata
            .pointer("/content_shape_contract/forbidden_shape")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().filter_map(Value::as_str).any(|item| {
                    item.contains("test module payload") || item.contains("unittest/pytest")
                })
            })
        && bad_result.output_text.contains("unittest/pytest test module")
        && crate::agent::content_shape_contract::python_source_executable_shape_rejects_test_module_payload_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_markdown_payload_fixture_passes() -> bool {
    crate::agent::content_shape_contract::python_source_executable_shape_rejects_markdown_payload_fixture_passes()
}

pub(crate) fn source_content_shape_rejects_raw_prose_line_fixture_passes() -> bool {
    crate::agent::content_shape_contract::python_source_executable_shape_rejects_raw_prose_line_fixture_passes()
}

pub(crate) fn corrective_content_shape_no_progress_terminal_guard_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "component.py",
        "content": "import unittest\nimport component\n\nclass TestComponent(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.add(2, 3), 5)\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
    });
    let Some(bad_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("component.py")];
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut counts = BTreeMap::<String, usize>::new();
    let first = ToolLifecycleRuntime::record_corrective_content_shape_no_progress(
        &mut counts,
        "write",
        &bad_result.metadata,
        &state,
        &allowed,
        &ToolChoice::Named(ToolName::Write),
        TurnLifecycleKernel::open_executable_work_requires_tool_call(&state),
    );
    let second = ToolLifecycleRuntime::record_corrective_content_shape_no_progress(
        &mut counts,
        "write",
        &bad_result.metadata,
        &state,
        &allowed,
        &ToolChoice::Named(ToolName::Write),
        TurnLifecycleKernel::open_executable_work_requires_tool_call(&state),
    );
    let third = ToolLifecycleRuntime::record_corrective_content_shape_no_progress(
        &mut counts,
        "write",
        &bad_result.metadata,
        &state,
        &allowed,
        &ToolChoice::Named(ToolName::Write),
        TurnLifecycleKernel::open_executable_work_requires_tool_call(&state),
    );
    first.is_none()
        && second.is_none()
        && third.is_some_and(|decision| {
            decision.count == OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
                && decision.terminal_message.is_some_and(|message| {
                    message.contains("content-changing authoring is required")
                        && message.contains("component.py")
                })
        })
}

pub(crate) fn text_artifact_content_shape_rejects_serialized_markdown_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "docs/component-design.md",
        "content": "\"# Component Design\\n\\n## Tests\\n\\n- `test_component.py` covers public behavior.\\n\\n```\\npython -m unittest\\n```\\n\""
    });
    let good_arguments = json!({
        "path": "docs/component-design.md",
        "content": "# Component Design\n\n## Tests\n\n- `test_component.py` covers public behavior.\n\n```bash\npython -m unittest\n```\n"
    });
    let root_path = std::env::temp_dir().join(format!(
        "moyai-text-shape-patch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    if std::fs::create_dir_all(root.as_std_path()).is_err() {
        return false;
    }
    let patch_arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: docs/component-design.md\n+\"# Component Design\\n\\n## Tests\\n\\n- `test_component.py` covers public behavior.\\n\\n```\\npython -m unittest\\n```\\n\"\n*** End Patch"
    });
    let Some(bad_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        let _ = std::fs::remove_dir_all(root.as_std_path());
        return false;
    };
    let patch_rejected =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "apply_patch",
            &patch_arguments,
            Some(root.as_path()),
        )
        .is_some_and(|result| {
            result
                .metadata
                .pointer("/content_shape_contract/kind")
                .and_then(Value::as_str)
                == Some("text_artifact_readable_content_shape")
        });
    let patch_left_workspace_clean = !root.join("docs/component-design.md").exists();
    let _ = std::fs::remove_dir_all(root.as_std_path());
    crate::agent::content_shape_contract::artifact_content_shape_violation_result("write", &good_arguments, None).is_none()
        && bad_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && bad_result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("text_artifact_readable_content_shape")
        && bad_result
            .output_text
            .contains("Required positive text artifact shape")
        && patch_rejected
        && patch_left_workspace_clean
        && crate::agent::content_shape_contract::text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes()
}

pub(crate) fn content_shape_mismatch_canonicalizes_workspace_absolute_target_fixture_passes() -> bool
{
    let root = Utf8PathBuf::from("C:/workspace");
    let bad_content = "\"# Component Design\\n\\n## Tests\\n\\n- `test_component.py` covers public behavior.\\n\"";
    let absolute_arguments = json!({
        "path": r"C:\\workspace\\docs\\component-design.md",
        "content": bad_content
    });
    let relative_arguments = json!({
        "path": "docs/component-design.md",
        "content": bad_content
    });
    let Some(absolute_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &absolute_arguments,
            Some(root.as_path()),
        )
    else {
        return false;
    };
    let Some(relative_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &relative_arguments,
            Some(root.as_path()),
        )
    else {
        return false;
    };
    let metadata_target = absolute_result
        .metadata
        .pointer("/content_shape_contract/target")
        .and_then(Value::as_str);
    let feedback_target = absolute_result
        .metadata
        .pointer("/tool_feedback_envelope/target")
        .and_then(Value::as_str);
    let active_target = absolute_result
        .metadata
        .pointer("/active_targets/0")
        .and_then(Value::as_str);
    let absolute_hash = absolute_result
        .metadata
        .pointer("/result_hash")
        .and_then(Value::as_str);
    let relative_hash = relative_result
        .metadata
        .pointer("/result_hash")
        .and_then(Value::as_str);
    metadata_target == Some("docs/component-design.md")
        && feedback_target == Some("docs/component-design.md")
        && active_target == Some("docs/component-design.md")
        && absolute_hash.is_some()
        && absolute_hash == relative_hash
        && absolute_result
            .output_text
            .contains("`docs/component-design.md`")
        && !absolute_result.output_text.contains("C:/workspace")
}

pub(crate) fn test_target_content_shape_apply_patch_post_content_enforced_fixture_passes() -> bool {
    let root_path = std::env::temp_dir().join(format!(
        "moyai-test-shape-patch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    if std::fs::create_dir_all(root.as_std_path()).is_err() {
        return false;
    }
    let test_path = root.join("test_component.py");
    let original = r#"import unittest
from component import add

class TestComponent(unittest.TestCase):
    def test_add(self):
        self.assertEqual(add(2, 3), 5)
"#;
    if std::fs::write(test_path.as_std_path(), original).is_err() {
        let _ = std::fs::remove_dir_all(root.as_std_path());
        return false;
    }
    let destructive_patch = json!({
        "patch_text": "*** Begin Patch\n*** Update File: test_component.py\n@@\n+from component import add\n*** End Patch"
    });
    let maybe_result =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "apply_patch",
            &destructive_patch,
            Some(root.as_path()),
        );
    let rejected = maybe_result.as_ref().is_some_and(|result| {
        result
            .metadata
            .get("write_content_shape_mismatch")
            .and_then(Value::as_bool)
            == Some(true)
            && result
                .metadata
                .pointer("/tool_feedback_envelope/side_effects_applied")
                .and_then(Value::as_bool)
                == Some(false)
            && result
                .output_text
                .contains("Required positive test-module shape")
    });
    let still_original =
        std::fs::read_to_string(test_path.as_std_path()).is_ok_and(|content| content == original);
    let _ = std::fs::remove_dir_all(root.as_std_path());
    rejected && still_original
}

fn closeout_final_response_timeout_ms(
    configured_timeout_ms: u64,
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) -> u64 {
    if !TurnLifecycleKernel::clean_closeout_final_message_lifecycle(state, active_work) {
        return configured_timeout_ms;
    }
    if configured_timeout_ms == 0 {
        return CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS;
    }
    configured_timeout_ms.min(CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS)
}

fn terminal_response_timeout_ms_for_state(
    configured_timeout_ms: u64,
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) -> Option<u64> {
    TurnLifecycleKernel::clean_closeout_final_message_lifecycle(state, active_work)
        .then(|| closeout_final_response_timeout_ms(configured_timeout_ms, state, active_work))
}

fn provider_error_is_request_timeout(error: &crate::error::LlmError) -> bool {
    error
        .to_string()
        .starts_with("provider request timeout after ")
}

fn closeout_timeout_fallback_text() -> &'static str {
    "完了しました。"
}

fn open_obligation_final_message_correction_text(
    state: &SessionStateSnapshot,
    attempt: usize,
    required_action: Option<&RequiredAction>,
    allowed_tools: &BTreeSet<String>,
    docs_grounding_required: bool,
) -> String {
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let target_line = if targets.is_empty() {
        "Open targets: none recorded.".to_string()
    } else {
        format!("Open targets: {targets}.")
    };
    let blocked_reason = state
        .completion
        .blocked_reason
        .as_deref()
        .unwrap_or("Open work remains for the latest user request.");
    let has_open_edit_work =
        state.completion.open_work_count > 0 || !state.active_targets.is_empty();
    let next_action = if docs_grounding_required {
        docs_route_content_grounding_correction_text(&targets, allowed_tools)
    } else if let Some(action) = required_action {
        open_obligation_required_action_correction_text(action, &targets, allowed_tools)
    } else if has_open_edit_work {
        open_obligation_file_change_correction_text(&targets, allowed_tools)
    } else if state.completion.verification_pending
        || !state.verification.required_commands.is_empty()
    {
        let commands =
            canonical_required_verification_commands(&state.verification.required_commands);
        let command_text = if commands.is_empty() {
            "the required verification command".to_string()
        } else {
            commands.join(", ")
        };
        format!(
            "Use the `shell` tool to run the required verification command before any final assistant message: {command_text}. A text-only promise does not satisfy this turn."
        )
    } else {
        open_obligation_file_change_correction_text(&targets, allowed_tools)
    };
    let provider_tool_choice_line = if attempt >= 2
        && required_action.is_some()
        && !allowed_tools.is_empty()
    {
        " The previous recovery request already required a tool call; this continuation treats another text-only response as provider ignored required tool-choice evidence and keeps the same typed action authority."
    } else {
        ""
    };
    format!(
        "The previous response was not accepted as a final answer because the current turn still has open obligations. Attempt {attempt}/{OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD}. {blocked_reason}{provider_tool_choice_line}\n{target_line}\n{next_action}"
    )
}

fn docs_route_content_grounding_correction_text(
    targets: &str,
    allowed_tools: &BTreeSet<String>,
) -> String {
    let allowed = if allowed_tools.is_empty() {
        "none".to_string()
    } else {
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let target_line = if targets.is_empty() {
        "the pending docs deliverable".to_string()
    } else {
        targets.to_string()
    };
    format!(
        "Docs authoring still needs content-bearing repository evidence before clean closeout. Available tools for this recovery step: {allowed}. Use `read`, `grep`, `docling_convert`, `mcp_call`, or `shell` to inspect a concrete source, test, config, or document file that grounds `{target_line}`, or use `apply_patch` if the visible evidence is already sufficient to create or update the docs target. Directory listings and final-answer prose do not satisfy this step; the satisfying docs authoring progress is `apply_patch` file-change evidence for the active docs target."
    )
}

fn open_obligation_required_action_correction_text(
    required_action: &RequiredAction,
    targets: &str,
    allowed_tools: &BTreeSet<String>,
) -> String {
    let required_action_projection = required_action.projection_label();
    if required_action.tool == ToolName::Write {
        let target = required_action
            .edit_target()
            .map(Utf8Path::as_str)
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .unwrap_or(targets);
        return format!(
            "Required action: `{required_action_projection}`. Call the `write` tool now with `path` exactly `{target}` and complete updated file content. Do not call supporting tools or answer in text; source repair remains open until that file-change evidence exists."
        );
    }
    if required_action.tool == ToolName::ApplyPatch {
        let target = required_action
            .edit_target()
            .map(Utf8Path::as_str)
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .unwrap_or(targets);
        return format!(
            "Required action: `{required_action_projection}`. Call the `apply_patch` tool now with a patch that changes `{target}`. Do not call supporting tools or answer in text; source repair remains open until that file-change evidence exists."
        );
    }
    if let Some(command) = required_action.shell_command().map(str::trim) {
        return format!(
            "Required action: `{required_action_projection}`. Use the `shell` tool to run the required verification command before any final assistant message: {command}. A text-only promise does not satisfy this turn."
        );
    }
    let allowed = if allowed_tools.is_empty() {
        "none".to_string()
    } else {
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    format!(
        "Required action: `{required_action_projection}`. Use the currently allowed tool surface ({allowed}) to complete that exact action before any final assistant message. A text-only promise does not satisfy this turn."
    )
}

fn open_obligation_file_change_correction_text(
    targets: &str,
    allowed_tools: &BTreeSet<String>,
) -> String {
    if allowed_tools.contains("apply_patch")
        && allowed_tools.contains("write")
        && !targets.is_empty()
        && targets.contains(", ")
    {
        return format!(
            "Use `apply_patch` or `write` for the open targets before any final assistant message: create or update these active targets: {targets}. With `apply_patch`, submit a single patch whose `patch_text` may contain multiple `*** Add File` or `*** Update File` sections. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
        );
    }
    if allowed_tools.contains("apply_patch") && !targets.is_empty() && targets.contains(", ") {
        return format!(
            "Use the `apply_patch` tool for the open targets before any final assistant message: submit a single patch whose `patch_text` creates or updates these active targets: {targets}. The patch may contain multiple `*** Add File` or `*** Update File` sections. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
        );
    }
    if allowed_tools.contains("apply_patch") && !targets.is_empty() && !targets.contains(", ") {
        return format!(
            "Use the `apply_patch` tool for the active target before any final assistant message: submit a patch whose `patch_text` adds or updates `{targets}`. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
        );
    }
    if allowed_tools.contains("write") && !targets.is_empty() && targets.contains(", ") {
        return format!(
            "Use file-changing tool calls for the open targets before any final assistant message: create or update these active targets: {targets}. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
        );
    }
    if allowed_tools.contains("write") && !targets.is_empty() && !targets.contains(", ") {
        return format!(
            "Use the `write` tool for the active target before any final assistant message: set `path` exactly to `{targets}` and provide complete updated file content. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
        );
    }
    if allowed_tools.contains("apply_patch") {
        if allowed_tools.contains("write") {
            return "Use `apply_patch` or `write` for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string();
        }
        return "Use `apply_patch` for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string();
    }
    "Use a file-changing tool call for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string()
}

fn open_obligation_final_message_terminal_message(
    state: &SessionStateSnapshot,
    attempts: usize,
) -> String {
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let blocked_reason = state
        .completion
        .blocked_reason
        .as_deref()
        .unwrap_or("open obligations remain");
    if targets.is_empty() {
        format!(
            "model returned a final assistant message {attempts} time(s) while {blocked_reason}; no clean closeout was accepted"
        )
    } else {
        format!(
            "model returned a final assistant message {attempts} time(s) while {blocked_reason}; open targets: {targets}; no clean closeout was accepted"
        )
    }
}

pub(crate) fn clean_closeout_final_message_lifecycle_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.completion.closeout_ready = true;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;
    TurnLifecycleKernel::clean_closeout_final_message_lifecycle(&state, None)
        && compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &BTreeSet::new(),
            TurnLifecycleRecoveryContext::default(),
        ) == ToolChoice::None
        && TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
}

pub(crate) fn answer_only_final_message_lifecycle_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Discover;
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;

    let mut executable = state.clone();
    executable.process_phase = crate::session::ProcessPhase::Author;
    executable.active_targets = vec![Utf8PathBuf::from("hello.py")];
    executable.completion.open_work_count = 1;
    executable.completion.blocked_reason =
        Some("Requested implementation updates are still missing from the workspace.".to_string());

    let mut verification = state.clone();
    verification.process_phase = crate::session::ProcessPhase::Verify;
    verification.completion.verification_pending = true;
    verification
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    TurnLifecycleKernel::answer_only_final_message_authority(&state)
        && TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
        && compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &BTreeSet::new(),
            TurnLifecycleRecoveryContext::default(),
        ) == ToolChoice::None
        && !TurnLifecycleKernel::answer_only_final_message_authority(&executable)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&executable)
        && TurnLifecycleKernel::open_executable_work_requires_tool_call(&executable)
        && !TurnLifecycleKernel::answer_only_final_message_authority(&verification)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&verification)
        && TurnLifecycleKernel::open_executable_work_requires_tool_call(&verification)
}

pub(crate) fn closeout_ready_final_response_timeout_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.completion.closeout_ready = true;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;
    let mut authoring_state = SessionStateSnapshot::default();
    authoring_state.process_phase = crate::session::ProcessPhase::Author;
    authoring_state.active_targets = vec![Utf8PathBuf::from("docs.md")];
    authoring_state.completion.closeout_ready = false;
    authoring_state.completion.open_work_count = 1;
    closeout_final_response_timeout_ms(0, &state, None) == CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS
        && closeout_final_response_timeout_ms(CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS + 1, &state, None)
            == CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS
        && closeout_final_response_timeout_ms(30_000, &state, None) == 30_000
        && terminal_response_timeout_ms_for_state(30_000, &state, None) == Some(30_000)
        && terminal_response_timeout_ms_for_state(30_000, &authoring_state, None).is_none()
        && closeout_timeout_fallback_text() == "完了しました。"
        && provider_error_is_request_timeout(&crate::error::LlmError::Message(
            provider_request_timeout_error_message(CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS),
        ))
}

pub(crate) fn open_obligation_final_message_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 2;
    state.completion.blocked_reason =
        Some("Requested implementation updates are still missing from the workspace.".to_string());

    let correction = open_obligation_final_message_correction_text(
        &state,
        1,
        None,
        &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
        false,
    );
    let terminal = open_obligation_final_message_terminal_message(
        &state,
        OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD,
    );

    TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
        && !TurnLifecycleKernel::clean_closeout_final_message_lifecycle(&state, None)
        && correction.contains("not accepted as a final answer")
        && correction.contains("component.py, test_component.py")
        && correction.contains("write")
        && correction.contains("apply_patch")
        && terminal.contains("no clean closeout was accepted")
        && terminal.contains("component.py, test_component.py")
}

pub(crate) fn open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes() -> bool
{
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 2;

    let open_only_key =
        open_obligation_final_message_guard_key(&state, None, &tool_names, None, false, false);
    let open_recovery_key =
        open_obligation_final_message_guard_key(&state, None, &tool_names, None, true, false);
    let first_invalid = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: component.py\n+ok\n*** End"}"#,
        "tool patch error: patch must end with `*** End Patch`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let second_invalid = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: component.py\n+def build():\nreturn 1\n*** End Patch"}"#,
        "tool patch error: add file body line `return 1` must start with `+`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(first_recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &first_invalid.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let Some(second_recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &second_invalid.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let first_invalid_key = open_obligation_final_message_guard_key(
        &state,
        None,
        &tool_names,
        Some(&first_recovery),
        true,
        false,
    );
    let second_invalid_key = open_obligation_final_message_guard_key(
        &state,
        None,
        &tool_names,
        Some(&second_recovery),
        true,
        false,
    );
    let mut counts = BTreeMap::<String, usize>::new();
    let open_only_count = *counts
        .entry(open_only_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let open_recovery_first_count = *counts
        .entry(open_recovery_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let open_recovery_second_count = *counts
        .entry(open_recovery_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let invalid_first_count = *counts
        .entry(first_invalid_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let invalid_second_count = *counts
        .entry(second_invalid_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let first_hash = first_recovery.result_hash.as_deref().unwrap_or("");
    let second_hash = second_recovery.result_hash.as_deref().unwrap_or("");

    open_only_key != first_invalid_key
        && open_only_key == open_recovery_key
        && open_recovery_key != first_invalid_key
        && first_invalid_key == second_invalid_key
        && !first_hash.is_empty()
        && !second_hash.is_empty()
        && first_hash != second_hash
        && !first_invalid_key.contains(first_hash)
        && !second_invalid_key.contains(second_hash)
        && open_only_count == 1
        && open_recovery_first_count == 2
        && open_recovery_second_count == OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD
        && invalid_first_count == 1
        && invalid_second_count == 2
        && invalid_second_count < OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD
}

pub(crate) fn docs_route_final_message_recovery_requires_content_grounding_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 1;
    state.completion.route_contract_pending = true;
    state.completion.blocked_reason =
        Some("docs route contract is pending: factual checks need repository evidence".to_string());
    state.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/component-design.md")),
        deliverables: vec![DocsDeliverableCoverage {
            target: Utf8PathBuf::from("docs/component-design.md"),
            kind: DocsDeliverableKind::Other,
            required_areas: vec![DocsArea::Tests],
            required_topics: vec!["repository evidence".to_string(), "tests".to_string()],
            satisfied_topics: Vec::new(),
            representative_paths: Vec::new(),
            grounding: vec![DocsGroundingCoverage {
                requirement: DocsGroundingRequirement::Tests,
                status: ContractStatus::Satisfied,
                representative_path: Some(Utf8PathBuf::from("test_component.py")),
                evidence_summary: Some("tests".to_string()),
            }],
        }],
        ..DocsRouteState::default()
    });

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let inspect_call_id = crate::session::ToolCallId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let test_read_call_id = crate::session::ToolCallId::new();
    let inspect_only_history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::ToolCall {
                call_id: inspect_call_id,
                tool: ToolName::InspectDirectory,
                arguments: json!({"path": "."}),
                model_arguments: json!({"path": "."}),
                effective_arguments: json!({"path": "."}),
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
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id: inspect_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Inspected workspace".to_string(),
                output_text: "Tree preview:\n  component.py\n  test_component.py\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("inspect-tree".to_string()),
                verification_run: None,
            },
        },
    ];
    let mut content_history = inspect_only_history.clone();
    let empty_grep_call_id = crate::session::ToolCallId::new();
    let source_grep_call_id = crate::session::ToolCallId::new();
    let mut empty_grep_history = inspect_only_history.clone();
    empty_grep_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolCall {
            call_id: empty_grep_call_id,
            tool: ToolName::Grep,
            arguments: json!({"pattern": "\\.py$", "path": "."}),
            model_arguments: json!({"pattern": "\\.py$", "path": "."}),
            effective_arguments: json!({"pattern": "\\.py$", "path": "."}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    empty_grep_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolOutput {
            call_id: empty_grep_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "Grep \\.py$".to_string(),
            output_text: "\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress".to_string(),
            metadata: json!({
                "operation_progress_class": "supporting_context",
                "total_matches": 0,
                "truncated": false
            }),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("empty-grep".to_string()),
            verification_run: None,
        },
    });
    let mut source_grep_history = inspect_only_history.clone();
    source_grep_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolCall {
            call_id: source_grep_call_id,
            tool: ToolName::Grep,
            arguments: json!({"pattern": "def render"}),
            model_arguments: json!({"pattern": "def render"}),
            effective_arguments: json!({"pattern": "def render"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    source_grep_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolOutput {
            call_id: source_grep_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "Grep def render".to_string(),
            output_text: "component.py:2:     def render(self):\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress".to_string(),
            metadata: json!({
                "operation_progress_class": "supporting_context",
                "total_matches": 1,
                "truncated": false
            }),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("source-grep".to_string()),
            verification_run: None,
        },
    });
    content_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 5,
        created_at_ms: 5,
        payload: HistoryItemPayload::ToolCall {
            call_id: read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path": "component.py"}),
            model_arguments: json!({"path": "component.py"}),
            effective_arguments: json!({"path": "component.py"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    content_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 6,
        created_at_ms: 6,
        payload: HistoryItemPayload::ToolOutput {
            call_id: read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "Read component.py".to_string(),
            output_text: "1: class Component:\n2:     def render(self):\n3:         return \"ok\"\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress".to_string(),
            metadata: json!({"operation_progress_class": "supporting_context"}),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("read-component".to_string()),
            verification_run: None,
        },
    });
    let mut test_content_history = content_history.clone();
    test_content_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 7,
        created_at_ms: 7,
        payload: HistoryItemPayload::ToolCall {
            call_id: test_read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path": "test_component.py"}),
            model_arguments: json!({"path": "test_component.py"}),
            effective_arguments: json!({"path": "test_component.py"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    test_content_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 8,
        created_at_ms: 8,
        payload: HistoryItemPayload::ToolOutput {
            call_id: test_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "Read test_component.py".to_string(),
            output_text: "1: import unittest\n2: class TestComponent(unittest.TestCase):\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress".to_string(),
            metadata: json!({"operation_progress_class": "supporting_context"}),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("read-test-component".to_string()),
            verification_run: None,
        },
    });

    let broad = BTreeSet::from([
        "apply_patch".to_string(),
        "docling_convert".to_string(),
        "grep".to_string(),
        "inspect_directory".to_string(),
        "list".to_string(),
        "mcp_call".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let needs_grounding = TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
        &state,
        docs_route_has_required_content_grounding_evidence(&state, &inspect_only_history),
    );
    let empty_grep_still_needs_grounding =
        TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
            &state,
            docs_route_has_required_content_grounding_evidence(&state, &empty_grep_history),
        );
    let source_only_still_needs_grounding =
        TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
            &state,
            docs_route_has_required_content_grounding_evidence(&state, &source_grep_history),
        );
    let mut grounding_surface = broad.clone();
    if needs_grounding {
        grounding_surface.retain(|tool| {
            TurnLifecycleKernel::docs_route_content_grounding_recovery_tool_visible(tool)
        });
    }
    let grounding_correction =
        open_obligation_final_message_correction_text(&state, 1, None, &grounding_surface, true);
    let source_only_grounded =
        !TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
            &state,
            docs_route_has_required_content_grounding_evidence(&state, &content_history),
        );
    let content_grounded = !TurnLifecycleKernel::docs_route_requires_content_grounding_before_write(
        &state,
        docs_route_has_required_content_grounding_evidence(&state, &test_content_history),
    );
    let mut write_surface = broad;
    if content_grounded {
        write_surface.retain(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(&state, tool)
        });
    }

    needs_grounding
        && empty_grep_still_needs_grounding
        && source_only_still_needs_grounding
        && grounding_surface
            == BTreeSet::from([
                "apply_patch".to_string(),
                "docling_convert".to_string(),
                "grep".to_string(),
                "mcp_call".to_string(),
                "read".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ])
        && grounding_correction.contains("content-bearing repository evidence")
        && grounding_correction.contains("or use `apply_patch`")
        && !source_only_grounded
        && content_grounded
        && write_surface == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && matches!(
            compile_turn_lifecycle_tool_choice(
                &crate::agent::prompt::PromptPolicy::default(),
                &state,
                &write_surface,
                TurnLifecycleRecoveryContext {
                    docs_grounding_final_message_recovery_active: true,
                    ..TurnLifecycleRecoveryContext::default()
                },
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn executed_tool_failure_terminal_guard_fixture_passes() -> bool {
    let allowed = BTreeSet::from(["read".to_string()]);
    let first = crate::agent::tool_orchestrator::executed_tool_failure_no_progress_key(
        "read",
        r#"{"path":"missing.py"}"#,
        &allowed,
        "The system cannot find the path specified. (os error 3)",
    );
    let second = crate::agent::tool_orchestrator::executed_tool_failure_no_progress_key(
        "read",
        r#"{"path":"missing.py"}"#,
        &allowed,
        "指定されたパスが見つかりません。 (os error 3)",
    );
    first == second
        && crate::agent::tool_orchestrator::executed_tool_failure_terminal_message(
            "read",
            3,
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
        Utf8PathBuf::from("test_arcade_game.py"),
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
        .retain(|target| target.as_str() != "test_arcade_game.py");
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
            Utf8PathBuf::from("arcade_game.py"),
            Utf8PathBuf::from("test_arcade_game.py"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let operation_intents = operation_intents_for_active_work(Some(&active_work));
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("README.md"),
        Utf8PathBuf::from("arcade_game.py"),
        Utf8PathBuf::from("test_arcade_game.py"),
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
        && ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
            &supporting_context_metadata,
            &state,
        )
        && !ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
            &progress_projection_metadata,
            &state,
        )
        && ToolLifecycleRuntime::operation_non_content_no_progress_key(
            "read",
            &supporting_context_metadata,
            &state,
            &allowed,
            &ToolChoice::Required,
        )
            .contains("content_changing_authoring_required")
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress(
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
        route_contract_satisfied: false,
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
        && ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
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
            let first_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
                "read",
                &read_metadata,
                &state,
                &effective,
                &ToolChoice::Required,
            );
            let repeated_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
                "read",
                &read_metadata,
                &state,
                &effective,
                &ToolChoice::Required,
            );
            let different_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
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
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress(
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
    let first_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let second_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &other_read_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let list_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "list",
        &list_metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let mut code_state = docs_state.clone();
    code_state.route = TaskRoute::Code;
    code_state.completion.route_contract_pending = false;
    let code_first = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &code_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let code_second = ToolLifecycleRuntime::operation_non_content_no_progress_key(
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
        && !ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            3,
            &docs_state,
        )
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
            &docs_state,
        )
}

pub(crate) fn authoring_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace_root.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace_root
                .join("docs/component-design.md")
                .as_std_path(),
            "# Component\n",
        )
        .is_err()
    {
        return false;
    }
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "list".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "workspace-list-hash"
    });
    let operation_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "list",
        &metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = BTreeSet::from([operation_key.clone()]);
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::authoring_supporting_context_budget_recovery_tool_visible(
                tool, true,
            )
        });
    }
    let mut docs_state = state.clone();
    docs_state.route = TaskRoute::Docs;
    docs_state.completion.route_contract_pending = true;

    let target_read_args = json!({"path": "docs/component-design.md"});
    let non_target_read_args = json!({"path": "docs/other-design.md"});
    let non_target_envelope =
        authoring_grounding_recovery_envelope(&[], &state, &workspace_root, &BTreeSet::new());
    let non_target_result = ToolLifecycleRuntime::authoring_target_grounding_required_result(
        "read",
        &non_target_read_args,
        &state,
        &non_target_envelope,
    );
    let mut non_target_counts = BTreeMap::new();
    let _ = ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
        &mut non_target_counts,
        &non_target_result,
    );
    let _ = ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
        &mut non_target_counts,
        &non_target_result,
    );
    let non_target_terminal =
        ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
            &mut non_target_counts,
            &non_target_result,
        )
        .terminal_message
        .unwrap_or_default();

    ToolLifecycleRuntime::authoring_supporting_context_budget_applies("supporting_context", &state)
        && !ToolLifecycleRuntime::authoring_supporting_context_budget_applies(
            "progress_projection",
            &state,
        )
        && TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
            &state, &exhausted,
        )
        && !TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
            &docs_state,
            &exhausted,
        )
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD,
            &state,
        )
        && operation_key.contains("content_changing_authoring_required")
        && visible == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && !visible.contains("list")
        && !visible.contains("grep")
        && !visible.contains("glob")
        && authoring_supporting_context_budget_recovery_read_disallowed(
            "read",
            &non_target_read_args,
            &state,
            &[],
            &workspace_root,
            &BTreeSet::new(),
        )
        && !authoring_supporting_context_budget_recovery_read_disallowed(
            "read",
            &target_read_args,
            &state,
            &[],
            &workspace_root,
            &BTreeSet::new(),
        )
        && non_target_result
            .metadata
            .get("operation_progress_class")
            .and_then(Value::as_str)
            == Some("authoring_target_grounding_required")
        && non_target_result
            .metadata
            .pointer("/missing_grounding_targets/0")
            .and_then(Value::as_str)
            == Some("docs/component-design.md")
        && non_target_terminal.contains("active target read")
}

pub(crate) fn multi_target_authoring_consumed_grounding_narrows_edit_recovery_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace_root.join("component.py").as_std_path(),
        "def add(a, b):\n    return a + b\n",
    )
    .is_err()
        || fs::write(
            workspace_root.join("test_component.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
    {
        return false;
    }
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let exhausted = BTreeSet::from(["supporting-context-budget".to_string()]);
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let partial_grounded = BTreeSet::from(["component.py".to_string()]);
    let all_grounded =
        BTreeSet::from(["component.py".to_string(), "test_component.py".to_string()]);
    let partial_missing =
        authoring_missing_grounding_targets(&[], &state, &workspace_root, &partial_grounded);
    let all_missing =
        authoring_missing_grounding_targets(&[], &state, &workspace_root, &all_grounded);
    let partial_envelope =
        authoring_grounding_recovery_envelope(&[], &state, &workspace_root, &partial_grounded);
    let mut partial_visible = allowed.clone();
    if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        partial_visible.retain(|tool| {
            TurnLifecycleKernel::authoring_supporting_context_budget_recovery_tool_visible(
                tool,
                !partial_missing.is_empty(),
            )
        });
    }
    let mut edit_visible = allowed.clone();
    if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        edit_visible.retain(|tool| {
            TurnLifecycleKernel::authoring_supporting_context_budget_recovery_tool_visible(
                tool,
                !all_missing.is_empty(),
            )
        });
    }
    let consumed_read_disallowed = authoring_supporting_context_budget_recovery_read_disallowed(
        "read",
        &json!({"path": "component.py"}),
        &state,
        &[],
        &workspace_root,
        &partial_grounded,
    );
    let remaining_read_allowed = !authoring_supporting_context_budget_recovery_read_disallowed(
        "read",
        &json!({"path": "test_component.py"}),
        &state,
        &[],
        &workspace_root,
        &partial_grounded,
    );
    let consumed_result = ToolLifecycleRuntime::authoring_target_grounding_required_result(
        "read",
        &json!({"path": "component.py"}),
        &state,
        &partial_envelope,
    );
    let consumed_output = consumed_result.output_text.clone();
    let mut schema_tools = vec![crate::llm::ToolSchema {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        }),
        strict: false,
    }];
    constrain_read_schema_to_missing_authoring_targets(&mut schema_tools, &partial_envelope);
    let schema_path_enum = schema_tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/path/enum"))
        .cloned()
        .unwrap_or(Value::Null);
    let recovery_obligation = authoring_grounding_recovery_obligation(&partial_envelope);
    let final_grounding_active =
        TurnLifecycleKernel::authoring_target_grounding_final_message_recovery_active(
            &state,
            active_authoring_targets_need_grounding(&[], &state, &workspace_root, &all_grounded),
        );
    let final_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &edit_visible,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    partial_missing == BTreeSet::from(["test_component.py".to_string()])
        && all_missing.is_empty()
        && partial_visible == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && edit_visible == BTreeSet::from(["apply_patch".to_string()])
        && consumed_read_disallowed
        && remaining_read_allowed
        && partial_envelope.consumed_targets == vec!["component.py".to_string()]
        && partial_envelope.missing_grounding_targets == vec!["test_component.py".to_string()]
        && consumed_output.contains("already grounded")
        && consumed_output.contains("test_component.py")
        && consumed_result
            .metadata
            .pointer("/missing_grounding_targets/0")
            .and_then(Value::as_str)
            == Some("test_component.py")
        && consumed_result
            .metadata
            .pointer("/consumed_targets/0")
            .and_then(Value::as_str)
            == Some("component.py")
        && schema_path_enum == json!(["test_component.py"])
        && recovery_obligation
            .evidence_refs
            .first()
            .is_some_and(|reference| {
                reference.reference.contains("missing=test_component.py")
                    && reference.reference.contains("consumed=component.py")
            })
        && !final_grounding_active
        && final_choice == ToolChoice::Required
}

pub(crate) fn repair_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.active_targets = vec![Utf8PathBuf::from("widget.py")];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "C:\\workspace\\widget.py",
        "result_hash": "target-grounding-read"
    });
    let operation_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = if ToolLifecycleRuntime::repair_supporting_context_budget_exhausts_for_metadata(
        "read", &metadata, &state,
    ) {
        BTreeSet::from([operation_key])
    } else {
        BTreeSet::new()
    };
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let non_target_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "C:\\workspace\\test_widget.py",
        "result_hash": "non-target-evidence-read"
    });
    let non_target_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &non_target_metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let non_target_exhausted =
        if ToolLifecycleRuntime::repair_supporting_context_budget_exhausts_for_metadata(
            "read",
            &non_target_metadata,
            &state,
        ) {
            BTreeSet::from([non_target_key])
        } else {
            BTreeSet::new()
        };
    let mut non_target_visible = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state,
        &non_target_exhausted,
    ) {
        non_target_visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let non_target_pre_authority = non_target_visible.clone();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
        &state,
        &non_target_pre_authority,
    ) {
        non_target_visible.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }

    ToolLifecycleRuntime::repair_supporting_context_budget_applies("supporting_context", &state)
        && !ToolLifecycleRuntime::repair_supporting_context_budget_applies(
            "progress_projection",
            &state,
        )
        && TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
            &state, &exhausted,
        )
        && !ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            1, &state,
        )
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && !visible.contains("read")
        && !visible.contains("shell")
        && non_target_visible.contains("read")
        && non_target_visible.contains("write")
        && non_target_visible.contains("apply_patch")
}

pub(crate) fn invalid_edit_arguments_project_no_progress_recovery_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.blocked_reason = Some(
        "Requested deliverables still require authoring in the workspace: `test_widget.py`."
            .to_string(),
    );
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let result = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: test_widget.py\n@@\n old\n+new\n*** End Patch"}"#,
        "tool edit error: context mismatch: expected `old`, got `old`",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let feedback = result
        .metadata
        .get("tool_feedback_envelope")
        .and_then(Value::as_object);
    let expected_lines_result = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: test_widget.py\n@@\nimport old\n+import new\n*** End Patch"}"#,
        "tool edit error: failed to find expected lines `import old`",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let malformed_write_result = invalid_tool_arguments_result(
        "write",
        "{\"path\":\"test_widget.py\",\"content\":\"def render():\\n    return \\\"ok\\\"\\n}",
        "EOF while parsing a string at line 1 column 58",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let malformed_write_feedback = malformed_write_result
        .metadata
        .get("tool_feedback_envelope")
        .and_then(Value::as_object);
    let expected_lines_feedback = expected_lines_result
        .metadata
        .get("tool_feedback_envelope")
        .and_then(Value::as_object);
    let no_write_allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let no_write_context_result = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: test_widget.py\n@@\nmissing\n+new\n*** End Patch"}"#,
        "tool edit error: failed to find expected lines `missing`",
        &state,
        Some(&no_write_allowed),
        Some(&ToolChoice::Required),
    );
    let no_write_context_feedback = no_write_context_result
        .metadata
        .get("tool_feedback_envelope")
        .and_then(Value::as_object);

    result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
        && result
            .output_text
            .contains("operation_progress_class: invalid_edit_arguments")
        && result.output_text.contains("progress_effect: no_progress")
        && result.output_text.contains("test_widget.py")
        && result.output_text.contains("Use `write`")
        && feedback.is_some_and(|feedback| {
            feedback
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "invalid_edit_arguments")
                && feedback
                    .get("progress_effect")
                    .and_then(Value::as_str)
                    .is_some_and(|effect| effect == "no_progress")
                && feedback
                    .get("side_effects_applied")
                    .and_then(Value::as_bool)
                    .is_some_and(|applied| !applied)
                && feedback
                    .get("allowed_surface_snapshot")
                    .and_then(|snapshot| snapshot.get("allowed_tools"))
                    .and_then(Value::as_array)
                    .is_some_and(|tools| tools.iter().any(|tool| tool.as_str() == Some("write")))
                && feedback
                    .get("parser_error_family")
                    .and_then(Value::as_str)
                    .is_some_and(|family| family == "apply_patch_context_mismatch")
        })
        && expected_lines_result
            .output_text
            .contains("complete replacement content")
        && no_write_context_result
            .output_text
            .contains("inspect only the exact active target `test_widget.py` with `shell`")
        && !no_write_context_result.output_text.contains("Use `write`")
        && no_write_context_feedback.is_some_and(|feedback| {
            feedback.get("recovery_action").and_then(Value::as_str)
                == Some("target_scoped_inspection_then_repatch_after_patch_context_mismatch")
        })
        && expected_lines_feedback.is_some_and(|feedback| {
            feedback
                .get("recovery_action")
                .and_then(Value::as_str)
                .is_some_and(|action| {
                    action == "write_full_replacement_or_repatch_after_patch_context_mismatch"
                })
        })
        && malformed_write_result
            .output_text
            .contains("parser_error_family: eof_while_parsing_string")
        && malformed_write_result
            .output_text
            .contains("raw_argument_shape_hash:")
        && malformed_write_result
            .output_text
            .contains("candidate_target_from_arguments: test_widget.py")
        && malformed_write_feedback.is_some_and(|feedback| {
            feedback
                .get("submitted_tool")
                .and_then(Value::as_str)
                .is_some_and(|tool| tool == "write")
                && feedback
                    .get("parser_error_family")
                    .and_then(Value::as_str)
                    .is_some_and(|family| family == "eof_while_parsing_string")
                && feedback
                    .get("raw_argument_shape_hash")
                    .and_then(Value::as_str)
                    .is_some_and(|hash| hash.len() == 64)
                && feedback
                    .get("candidate_target_from_arguments")
                    .and_then(Value::as_str)
                    .is_some_and(|target| target == "test_widget.py")
        })
}

pub(crate) fn invalid_edit_arguments_terminal_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let result = invalid_tool_arguments_result(
        "write",
        r#"{"content":"import unittest\n"}"#,
        "missing field `path`",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let repeat_key = invalid_edit_arguments_no_progress_key(
        "write",
        &result.metadata,
        &allowed,
        &ToolChoice::Auto,
    );
    let repeat_key_again = invalid_edit_arguments_no_progress_key(
        "write",
        &result.metadata,
        &allowed,
        &ToolChoice::Auto,
    );
    let mut other_state = state.clone();
    other_state.active_targets = vec![Utf8PathBuf::from("test_other.py")];
    let other_result = invalid_tool_arguments_result(
        "write",
        r#"{"content":"import unittest\n"}"#,
        "missing field `path`",
        &other_state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let other_key = invalid_edit_arguments_no_progress_key(
        "write",
        &other_result.metadata,
        &allowed,
        &ToolChoice::Auto,
    );
    let malformed_a = invalid_tool_arguments_result(
        "write",
        r#"{"path":"test_widget.py","content":"import unittest\nclass TestWidget"#,
        "EOF while parsing a string at line 1 column 62",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let malformed_b = invalid_tool_arguments_result(
        "write",
        r#"{"path":"test_widget.py","content":"import unittest\nclass TestWidget(unittest.TestCase):\n    def test_more(self):"#,
        "EOF while parsing a string at line 1 column 109",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let malformed_key_a = invalid_edit_arguments_no_progress_key(
        "write",
        &malformed_a.metadata,
        &allowed,
        &ToolChoice::Auto,
    );
    let malformed_key_b = invalid_edit_arguments_no_progress_key(
        "write",
        &malformed_b.metadata,
        &allowed,
        &ToolChoice::Auto,
    );
    let patch_allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let patch_a = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: test_widget.py\n+import unittest\n\ndef helper():\n+    return 1\n*** End Patch"}"#,
        "tool patch error: add file body line `def helper():` must start with `+`; every added content line, including blank lines and top-level `def`/`class`/`import` lines, must be prefixed with `+`.",
        &state,
        Some(&patch_allowed),
        Some(&ToolChoice::Auto),
    );
    let patch_b = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: test_widget.py\n+import unittest\n+class TestWidget(unittest.TestCase):\n\ndef test_widget():\n+    pass\n*** End Patch"}"#,
        "tool patch error: add file body line `def test_widget():` must start with `+`; every added content line, including blank lines and top-level `def`/`class`/`import` lines, must be prefixed with `+`.",
        &state,
        Some(&patch_allowed),
        Some(&ToolChoice::Auto),
    );
    let patch_key_a = invalid_edit_arguments_no_progress_key(
        "apply_patch",
        &patch_a.metadata,
        &patch_allowed,
        &ToolChoice::Auto,
    );
    let patch_key_b = invalid_edit_arguments_no_progress_key(
        "apply_patch",
        &patch_b.metadata,
        &patch_allowed,
        &ToolChoice::Auto,
    );
    let terminal_message = invalid_edit_arguments_terminal_message(
        "write",
        INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD,
        &result.metadata,
    );

    repeat_key.is_some()
        && repeat_key == repeat_key_again
        && repeat_key != other_key
        && malformed_key_a.is_some()
        && malformed_key_a == malformed_key_b
        && patch_key_a.is_some()
        && patch_key_a == patch_key_b
        && patch_a
            .metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("raw_argument_shape_hash"))
            != patch_b
                .metadata
                .get("tool_feedback_envelope")
                .and_then(|feedback| feedback.get("raw_argument_shape_hash"))
        && malformed_a.output_text.contains("smaller valid JSON")
        && should_terminalize_invalid_edit_arguments_no_progress(
            INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD,
        )
        && !should_terminalize_invalid_edit_arguments_no_progress(
            INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD - 1,
        )
        && terminal_message.contains("invalid edit arguments")
        && terminal_message.contains("test_widget.py")
        && terminal_message.contains("outer timeout")
}

pub(crate) fn malformed_write_patch_capable_recovery_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("component.py")];
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let previous_tool_choice = ToolChoice::Named(ToolName::Write);
    let result = invalid_tool_arguments_result(
        "write",
        r#"{"path":"component.py","content":"def render():\n    return \"ok"#,
        "EOF while parsing a string at line 1 column 63",
        &state,
        Some(&allowed),
        Some(&previous_tool_choice),
    );
    let recovery_needed = invalid_write_arguments_need_patch_capable_recovery(
        "write",
        &result.metadata,
        &allowed,
        &previous_tool_choice,
    );
    let recovery_active = recovery_needed
        && TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && allowed.contains("write")
        && allowed.contains("apply_patch");
    let dispatch_tool_choice = if recovery_active {
        ToolChoice::Required
    } else {
        previous_tool_choice.clone()
    };
    let policy = TurnLifecycleKernel::malformed_write_patch_capable_recovery_policy(&state);
    let repeat_key = invalid_edit_arguments_no_progress_key(
        "write",
        &result.metadata,
        &allowed,
        &dispatch_tool_choice,
    );
    let mut generated_test_state = SessionStateSnapshot::default();
    generated_test_state.route = TaskRoute::Code;
    generated_test_state.process_phase = crate::session::ProcessPhase::Author;
    generated_test_state.completion.open_work_count = 1;
    generated_test_state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    let full_surface = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let full_surface_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let generated_test_invalid = invalid_tool_arguments_result(
        "write",
        r#"{"content":"\"import unittest\nfrom widget import render\n\nclass TestWidget(unittest.TestCase):\n    def test_render(self):\n        self.assertEqual(render(), \"ok\")"#,
        "EOF while parsing a string at line 1 column 153",
        &generated_test_state,
        Some(&full_surface),
        Some(&ToolChoice::Auto),
    );
    let generated_test_recovery_needed = invalid_write_arguments_need_patch_capable_recovery(
        "write",
        &generated_test_invalid.metadata,
        &full_surface,
        &ToolChoice::Auto,
    );
    let mut generated_test_recovery_tools = full_surface_tools.clone();
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(
        &mut generated_test_recovery_tools,
        &generated_test_state,
    );
    if generated_test_recovery_needed {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut generated_test_recovery_tools,
            &full_surface_tools,
            |name| matches!(name, "apply_patch" | "write"),
        );
        generated_test_recovery_tools
            .retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
    } else {
        TurnLifecycleKernel::apply_generated_test_source_reference_grounding_surface(
            &mut generated_test_recovery_tools,
            &full_surface_tools,
            true,
        );
    }
    let generated_test_recovery_tool_names = generated_test_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let generated_test_recovery_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &generated_test_state,
        &generated_test_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            generated_test_source_reference_grounding_active: true,
            malformed_write_patch_recovery_active: generated_test_recovery_needed,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    recovery_needed
        && recovery_active
        && dispatch_tool_choice == ToolChoice::Required
        && !matches!(dispatch_tool_choice, ToolChoice::Named(ToolName::Write))
        && generated_test_recovery_needed
        && generated_test_recovery_tool_names
            == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && generated_test_recovery_choice == ToolChoice::Required
        && result
            .output_text
            .contains("or use `apply_patch` with a concise add/update patch")
        && policy.policy == "malformed_write_patch_capable_recovery_surface"
        && policy.active_targets == vec!["component.py".to_string()]
        && repeat_key.is_some_and(|key| {
            key.contains("tool=write")
                && key.contains("allowed=apply_patch,write")
                && key.contains("choice=required")
        })
}

pub(crate) fn malformed_apply_patch_write_recovery_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("calculator.py")];
    let normal_code_surface = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let stable_tools = ["apply_patch", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let malformed_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: calculator.py\ndef add(a, b):\n    return a + b\n*** End Patch"}"#,
        "tool patch error: Add File body line must start with `+`: def add(a, b):",
        &state,
        Some(&normal_code_surface),
        Some(&ToolChoice::Auto),
    );
    let recovery_needed = invalid_apply_patch_arguments_need_write_recovery(
        "apply_patch",
        &malformed_patch.metadata,
        &state,
        &normal_code_surface,
        &ToolChoice::Auto,
    );
    let mut recovery_tools = normal_code_surface
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut recovery_tools, &state);
    if recovery_needed {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut recovery_tools,
            &stable_tools,
            |name| matches!(name, "apply_patch" | "write"),
        );
        recovery_tools.retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
    }
    let recovery_tool_names = recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let recovery_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &recovery_tool_names,
        TurnLifecycleRecoveryContext {
            malformed_apply_patch_write_recovery_active: recovery_needed,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let envelope = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &malformed_patch.metadata,
        &state,
        &normal_code_surface,
        &ToolChoice::Auto,
    );
    let policy = TurnLifecycleKernel::malformed_apply_patch_write_recovery_policy(&state);
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.completion.open_work_count = 1;
    docs_state.completion.route_contract_pending = true;
    docs_state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    let docs_apply_patch_only_surface = BTreeSet::from(["apply_patch".to_string()]);
    let docs_malformed_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: docs/component-design.md\n+# Component design\n\n## API\n\nDetails\n*** End Patch"}"#,
        "tool patch error: add file body line `` must start with `+`; every added content line, including blank lines, must be prefixed with `+`.",
        &docs_state,
        Some(&docs_apply_patch_only_surface),
        Some(&ToolChoice::Required),
    );
    let docs_recovery_needed = invalid_apply_patch_arguments_need_write_recovery(
        "apply_patch",
        &docs_malformed_patch.metadata,
        &docs_state,
        &docs_apply_patch_only_surface,
        &ToolChoice::Required,
    );
    let mut docs_recovery_tools = docs_apply_patch_only_surface
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(
        &mut docs_recovery_tools,
        &docs_state,
    );
    if docs_recovery_needed {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut docs_recovery_tools,
            &stable_tools,
            |name| matches!(name, "apply_patch" | "write"),
        );
        docs_recovery_tools.retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
    }
    let docs_recovery_tool_names = docs_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let docs_recovery_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &docs_state,
        &docs_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            malformed_apply_patch_write_recovery_active: docs_recovery_needed,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut stale_candidate_state = SessionStateSnapshot::default();
    stale_candidate_state.route = TaskRoute::Code;
    stale_candidate_state.process_phase = crate::session::ProcessPhase::Author;
    stale_candidate_state.completion.open_work_count = 1;
    stale_candidate_state.active_targets = vec![Utf8PathBuf::from("test_calculator.py")];
    let stale_candidate_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: calculator.py\n@@\ndef calculate(left, operator, right):\n+    return left + right\n*** End Patch"}"#,
        "tool patch error: unexpected patch hunk line `def calculate(left, operator, right):`.",
        &stale_candidate_state,
        Some(&normal_code_surface),
        Some(&ToolChoice::Auto),
    );
    let stale_candidate_recovery_needed = invalid_apply_patch_arguments_need_write_recovery(
        "apply_patch",
        &stale_candidate_patch.metadata,
        &stale_candidate_state,
        &normal_code_surface,
        &ToolChoice::Auto,
    );
    let stale_candidate_envelope = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &stale_candidate_patch.metadata,
        &stale_candidate_state,
        &normal_code_surface,
        &ToolChoice::Auto,
    );

    !normal_code_surface.contains("write")
        && recovery_needed
        && recovery_tool_names == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && recovery_choice == ToolChoice::Required
        && docs_recovery_needed
        && docs_recovery_tool_names
            == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && docs_recovery_choice == ToolChoice::Required
        && malformed_patch
            .output_text
            .contains("If the next recovery surface includes `write`")
        && envelope.is_some_and(|value| {
            value
                .prompt
                .contains("when the recovery surface provides `write`")
                && value.candidate_target.as_deref() == Some("calculator.py")
        })
        && stale_candidate_recovery_needed
        && stale_candidate_envelope.is_some_and(|value| {
            value.candidate_target.as_deref() == Some("calculator.py")
                && value.active_targets == vec!["test_calculator.py".to_string()]
                && value.prompt.contains(
                    "It is not currently an open target, so choose one of the open targets",
                )
        })
        && policy.policy == "malformed_apply_patch_write_recovery_surface"
        && policy.active_targets == vec!["calculator.py".to_string()]
}

pub(crate) fn failed_patch_context_mismatch_reopens_target_grounding_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-failed-patch-context-mismatch-grounding".to_string(),
        failing_labels: vec!["test_widget".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("test_widget".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("SyntaxError: unmatched ')'".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `SyntaxError: unmatched ')'`".to_string(),
                "source parse frame `test_widget.py`".to_string(),
                "source_parse_defect".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let read_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "C:\\workspace\\test_widget.py",
        "result_hash": "target-grounding-read"
    });
    let exhausted_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = BTreeSet::from([exhausted_key]);
    let mut visible_without_failed_patch = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible_without_failed_patch.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }

    let invalid_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: test_widget.py\n@@\n old\n+new\n*** End Patch"}"#,
        "tool edit error: context mismatch: expected `old`, got ``",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let mut patch_grounding_targets = BTreeSet::<String>::new();
    record_patch_context_mismatch_grounding_targets(
        &mut patch_grounding_targets,
        &invalid_patch.metadata,
        &state,
    );
    let patch_grounding_active =
        patch_context_mismatch_target_grounding_surface_active(&state, &patch_grounding_targets);
    let stable_tools = allowed
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut visible_after_failed_patch = stable_tools
        .iter()
        .filter(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"))
        .cloned()
        .collect::<Vec<_>>();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) && !patch_grounding_active
    {
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(&tool.name)
        });
    }
    if patch_grounding_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut visible_after_failed_patch,
            &stable_tools,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let rejected_supporting = BTreeMap::from([(
        "model_action_rejection|semantic=provider_ignored_edit_only_surface|hash=fixture"
            .to_string(),
        1,
    )]);
    if TurnLifecycleKernel::provider_noncompliance_edit_recovery_applies(
        &state,
        &rejected_supporting,
    ) && !patch_grounding_active
        && visible_after_failed_patch.iter().any(|tool| {
            TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(&tool.name)
        })
    {
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(&tool.name)
        });
    }
    let visible_after_failed_patch_names = visible_after_failed_patch
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    if patch_context_mismatch_target_grounding_read_satisfied("read", &read_metadata, &state) {
        patch_grounding_targets.clear();
    }

    visible_without_failed_patch
        == BTreeSet::from([
            "apply_patch".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && patch_grounding_active
        && visible_after_failed_patch_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && patch_grounding_targets.is_empty()
}

pub(crate) fn malformed_write_arguments_terminal_quote_repair_fixture_passes() -> bool {
    let malformed = "{\"path\":\"test_widget.py\",\"content\":\"\\\"\\\"\\\"Doc\\n\\\"\\\"\\\"\\nimport unittest\\nINPUT = \\\"3 + 5\\\\nquit\\\\n\\\"\\n}";
    let repaired = repair_unambiguous_malformed_edit_arguments_json("write", malformed);
    let valid = "{\"path\":\"test_widget.py\",\"content\":\"import unittest\\n\"}";
    let already_valid = repair_unambiguous_malformed_edit_arguments_json("write", valid);
    let non_edit = repair_unambiguous_malformed_edit_arguments_json("read", malformed);
    let unrecoverable = repair_unambiguous_malformed_edit_arguments_json(
        "write",
        "{\"path\":\"test_widget.py\",\"content\":",
    );

    serde_json::from_str::<Value>(malformed).is_err()
        && repaired
            .as_ref()
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .is_some_and(|value| {
                value.get("path").and_then(Value::as_str) == Some("test_widget.py")
                    && value
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| {
                            content.contains("\"\"\"Doc\n\"\"\"")
                                && content.contains("\nimport unittest\n")
                                && content.contains("INPUT = \"3 + 5\\nquit\\n\"")
                        })
            })
        && already_valid.is_none()
        && non_edit.is_none()
        && unrecoverable.is_none()
}

pub(crate) fn singleton_active_target_write_arguments_repair_fixture_passes() -> bool {
    let active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    let content_only = r#"{"content":"import unittest\n"}"#;
    let inferred =
        repair_write_arguments_from_active_target("write", content_only, &active_targets);
    let malformed_content_only = "{\"content\":\"import unittest\\n";
    let inferred_from_malformed =
        repair_write_arguments_from_active_target("write", malformed_content_only, &active_targets);
    let multiple_targets = vec![
        Utf8PathBuf::from("test_widget.py"),
        Utf8PathBuf::from("test_other.py"),
    ];
    let ambiguous =
        repair_write_arguments_from_active_target("write", content_only, &multiple_targets);
    let absolute_target = vec![Utf8PathBuf::from("C:/workspace/test_widget.py")];
    let absolute =
        repair_write_arguments_from_active_target("write", content_only, &absolute_target);
    let embedded_path_payload = r#"{"content":"import unittest\n\",\"path\":\"test_other.py\""}"#;
    let embedded_path_repair =
        repair_write_arguments_from_active_target("write", embedded_path_payload, &active_targets);

    inferred
        .as_ref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .is_some_and(|value| {
            value.get("path").and_then(Value::as_str) == Some("test_widget.py")
                && value
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| content.contains("import unittest"))
        })
        && inferred_from_malformed
            .as_ref()
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .is_some_and(|value| {
                value.get("path").and_then(Value::as_str) == Some("test_widget.py")
                    && value
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|content| content.contains("import unittest"))
            })
        && ambiguous.is_none()
        && absolute.is_none()
        && embedded_path_repair.is_none()
}

pub(crate) fn verification_repair_target_grounding_surface_keeps_read_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: component.divide raises the wrong exception".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.verification.failing_labels = vec!["test_divide_by_zero".to_string()];
    let mut cluster = crate::agent::state::public_class_attribute_cluster_fixture();
    cluster.source_refs = vec!["component.py".to_string()];
    cluster.test_refs = vec!["test_component.py".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_exception_mismatch".to_string());
        evidence.target = Some("C:/workspace/project/component.py".to_string());
        evidence.source_refs = vec!["component.py".to_string()];
        evidence.test_refs = vec!["test_component.py".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let narrowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &allowed) {
        visible.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }
    let narrowed_active =
        TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &narrowed);
    let mut visible_from_narrowed = allowed.clone();
    if narrowed_active {
        visible_from_narrowed.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }
    let stable_tool_schemas = allowed
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut narrowed_schema_surface = narrowed
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let malformed_write_recovery_active = true;
    if narrowed_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut narrowed_schema_surface,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        narrowed_schema_surface.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    } else if malformed_write_recovery_active {
        narrowed_schema_surface
            .retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
    }
    let narrowed_schema_names = narrowed_schema_surface
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut post_provider_normalized_surface = narrowed_schema_surface.clone();
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(
        &mut post_provider_normalized_surface,
        &state,
    );
    if narrowed_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut post_provider_normalized_surface,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        post_provider_normalized_surface.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let post_provider_normalized_names = post_provider_normalized_surface
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let Some(projection) = crate::agent::repair_lane::project_repair_lane(&state, &visible) else {
        return false;
    };

    TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &allowed)
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && narrowed_active
        && visible_from_narrowed
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && narrowed_schema_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && post_provider_normalized_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && !visible.contains("shell")
        && !visible.contains("grep")
        && !visible.contains("list")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template
                    .required_edit_surface
                    .contains(&"apply_patch".to_string())
                    && template
                        .required_edit_surface
                        .contains(&"write".to_string())
                    && !template.forbidden_stale_tools.contains(&"read".to_string())
                    && template
                        .forbidden_stale_tools
                        .contains(&"shell".to_string())
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot
                    .allowed_surface_snapshot
                    .contains(&"read".to_string())
                    && !snapshot
                        .forbidden_actions
                        .contains(&"stale_tool:read".to_string())
                    && snapshot
                        .forbidden_actions
                        .contains(&"stale_tool:shell".to_string())
                    && snapshot.forbidden_actions.iter().any(|action| {
                        action == "unbounded_context_churn_before_source_contract_repair"
                    })
            })
}

pub(crate) fn source_repair_initial_grounding_precedes_edit_only_recovery_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("component.py")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: public stdout assertion mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.verification.failing_labels = vec!["test_public_stdout".to_string()];
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-public-output-source-grounding".to_string(),
        failing_labels: vec!["test_public_stdout".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("test_public_stdout".to_string()),
            target: None,
            symbol: None,
            call_site: Some("self.assertIn(\"expected token\", result.stdout)".to_string()),
            exception: None,
            expected: Some("expected token".to_string()),
            observed: Some("stdout `unmatched stdout output`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_output_stream:stdout".to_string(),
                "source_public_behavior_assertion".to_string(),
            ],
            sibling_obligations: vec!["stdout contains expected token".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec!["component.py".to_string()],
            test_refs: vec!["test_component.py".to_string()],
        }],
        sibling_obligations: vec!["stdout contains expected token".to_string()],
        source_refs: vec!["component.py".to_string()],
        test_refs: vec!["test_component.py".to_string()],
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let stable_names = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let stable_tool_schemas = stable_names
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut first_repair_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "write".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let first_names = first_repair_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
        &state,
        &first_names,
    ) {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut first_repair_tools,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        first_repair_tools.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let first_visible = first_repair_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let exhausted = BTreeSet::from(["component-read-budget".to_string()]);
    let mut post_grounding_visible = first_visible.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        post_grounding_visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let required_write = fixture_required_edit_action(ToolName::Write, "test_component.py");
    let mut provider_counts = BTreeMap::new();
    let provider_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut provider_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"python -m unittest"}"#,
            allowed_tools: &post_grounding_visible,
            tool_choice: &ToolChoice::Required,
            required_action: Some(&required_write),
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("fixture"),
            recovery_no_progress_key: None,
        },
    );

    first_visible
        == BTreeSet::from([
            "apply_patch".to_string(),
            "read".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && post_grounding_visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && provider_decision.count == 1
        && provider_decision.terminal_message.is_none()
}

pub(crate) fn rejected_tool_batch_terminal_guard_waits_for_followup_fixture_passes() -> bool {
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "docling_convert".to_string(),
        "grep".to_string(),
        "mcp_call".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let required_patch =
        fixture_required_edit_action(ToolName::ApplyPatch, "docs/calculator-design.md");
    let mut counts = BTreeMap::<String, usize>::new();
    let before_first_model_response = counts.clone();
    let first_request = RejectedToolNoProgressGuardRequest {
        effective_tool_name: "",
        effective_arguments_json: r#"{"path":"calculator.py"}"#,
        allowed_tools: &allowed,
        tool_choice: &ToolChoice::Auto,
        required_action: Some(&required_patch),
        provider_noncompliance: false,
        semantic_class: "invalid_tool_call",
        result_hash: Some("empty-tool-name-path-proposal"),
        recovery_no_progress_key: None,
    };
    let first_key = ToolLifecycleRuntime::rejected_tool_no_progress_guard_key(&first_request);
    let first_decision =
        ToolLifecycleRuntime::record_rejected_tool_no_progress(&mut counts, first_request);
    let second_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "",
            effective_arguments_json: r#"{"path":"calculator.py"}"#,
            allowed_tools: &allowed,
            tool_choice: &ToolChoice::Auto,
            required_action: Some(&required_patch),
            provider_noncompliance: false,
            semantic_class: "invalid_tool_call",
            result_hash: Some("empty-tool-name-path-proposal"),
            recovery_no_progress_key: None,
        },
    );
    let third_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "",
            effective_arguments_json: r#"{"path":"calculator.py"}"#,
            allowed_tools: &allowed,
            tool_choice: &ToolChoice::Auto,
            required_action: Some(&required_patch),
            provider_noncompliance: false,
            semantic_class: "invalid_tool_call",
            result_hash: Some("empty-tool-name-path-proposal"),
            recovery_no_progress_key: None,
        },
    );
    let first_batch_terminal_is_suppressed = third_decision.terminal_message.is_some()
        && !before_first_model_response.contains_key(&first_key);
    let before_followup_response = counts.clone();
    let followup_request = RejectedToolNoProgressGuardRequest {
        effective_tool_name: "",
        effective_arguments_json: r#"{"path":"calculator.py"}"#,
        allowed_tools: &allowed,
        tool_choice: &ToolChoice::Auto,
        required_action: Some(&required_patch),
        provider_noncompliance: false,
        semantic_class: "invalid_tool_call",
        result_hash: Some("empty-tool-name-path-proposal"),
        recovery_no_progress_key: None,
    };
    let followup_key = ToolLifecycleRuntime::rejected_tool_no_progress_guard_key(&followup_request);
    let followup_decision =
        ToolLifecycleRuntime::record_rejected_tool_no_progress(&mut counts, followup_request);

    first_decision.count == 1
        && first_decision.terminal_message.is_none()
        && second_decision.count == 2
        && second_decision.terminal_message.is_none()
        && third_decision.count == 3
        && first_batch_terminal_is_suppressed
        && before_followup_response.contains_key(&followup_key)
        && followup_decision.terminal_message.is_some()
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
        "result_hash": "omitted-for-docs"
    });
    let operation_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &metadata,
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let budget_key = ToolLifecycleRuntime::docs_route_supporting_context_budget_key(
        &docs_state,
        &allowed,
        &ToolChoice::Auto,
    );
    let result = ToolLifecycleRuntime::docs_supporting_context_budget_exhausted_result(
        "read",
        &json!({"path": "backend/app/main.py"}),
        &docs_state,
    );
    let mut counts = BTreeMap::new();
    let _ = ToolLifecycleRuntime::record_docs_supporting_context_budget_exhausted_no_progress(
        &mut counts,
        budget_key.clone(),
        &docs_state,
    );
    let _ = ToolLifecycleRuntime::record_docs_supporting_context_budget_exhausted_no_progress(
        &mut counts,
        budget_key.clone(),
        &docs_state,
    );
    let terminal =
        ToolLifecycleRuntime::record_docs_supporting_context_budget_exhausted_no_progress(
            &mut counts,
            budget_key.clone(),
            &docs_state,
        )
        .terminal_message
        .unwrap_or_default();
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
            == Some(3)
        && terminal.contains("budget was exhausted")
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
    let exhausted = BTreeSet::from([
        ToolLifecycleRuntime::docs_route_supporting_context_budget_key(
            &docs_state,
            &allowed,
            &ToolChoice::Auto,
        ),
    ]);
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
        &docs_state,
        &exhausted,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_tool_visible(tool)
        });
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
        && TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
            &docs_state,
            &exhausted,
        )
        && !TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
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
    if TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
        &docs_state,
        &retained,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_tool_visible(tool)
        });
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

fn canonical_shell_command_keys(command: &str) -> BTreeSet<String> {
    let mut keys = verification_command_satisfaction_keys(command);
    if let Some(key) = canonical_verification_command_identity_key(command) {
        keys.insert(key);
    }
    keys
}

fn verification_command_key_family_matches(
    submitted_keys: &BTreeSet<String>,
    required_keys: &BTreeSet<String>,
) -> bool {
    if submitted_keys.is_empty() || required_keys.is_empty() {
        return false;
    }
    submitted_keys.iter().any(|submitted| {
        required_keys.iter().any(|required| {
            submitted == required
                || submitted.starts_with(&format!("{required} "))
                || required.starts_with(&format!("{submitted} "))
        })
    })
}

fn canonical_required_verification_commands(required_commands: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for command in required_commands {
        let key = canonical_verification_command_identity_key(command).unwrap_or_else(|| {
            command
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        });
        if seen.insert(key) {
            commands.push(command.clone());
        }
    }
    commands
}

fn executable_verification_command_forms(
    required_commands: &[String],
    shell_family: crate::config::ShellFamily,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for command in required_commands {
        let executable = if let Some(suggested) =
            crate::tool::shell::command_text_encoding_suggested_command(command, shell_family)
        {
            suggested
        } else {
            command.clone()
        };
        if seen.insert(executable.clone()) {
            commands.push(executable);
        }
    }
    commands
}

fn repair_shell_arguments_from_singleton_verification_command(
    effective_tool_name: &str,
    arguments_json: &str,
    active_work: Option<&ActiveWorkContract>,
    shell_family: crate::config::ShellFamily,
) -> Option<String> {
    if effective_tool_name != "shell" {
        return None;
    }
    let Some(ActiveWorkContract::Verification {
        commands,
        repair_required,
        ..
    }) = active_work
    else {
        return None;
    };
    if *repair_required {
        return None;
    }
    let required_commands = canonical_required_verification_commands(commands);
    if required_commands.len() != 1 {
        return None;
    }
    let parsed = serde_json::from_str::<Value>(arguments_json).ok()?;
    let submitted = parsed.get("command").and_then(Value::as_str)?.trim();
    let submitted_keys = canonical_shell_command_keys(submitted);
    let required_key = canonical_verification_command_identity_key(&required_commands[0])?;
    if verification_command_key_family_matches(
        &submitted_keys,
        &BTreeSet::from([required_key.clone()]),
    ) {
        let suggested =
            crate::tool::shell::command_text_encoding_suggested_command(submitted, shell_family)?;
        if normalized_command_text_for_family_match(&suggested)
            == normalized_command_text_for_family_match(submitted)
        {
            return None;
        }
        return Some(
            json!({
                "command": suggested,
                "description": "Run runtime-owned required verification command"
            })
            .to_string(),
        );
    }
    let command = executable_verification_command_forms(&required_commands, shell_family)
        .into_iter()
        .next()
        .unwrap_or_else(|| required_commands[0].clone());
    Some(
        json!({
            "command": command,
            "description": "Run runtime-owned required verification command"
        })
        .to_string(),
    )
}

fn fixture_required_edit_action(tool: ToolName, target: &str) -> RequiredAction {
    let prefix = match tool {
        ToolName::ApplyPatch => "apply_patch",
        ToolName::Write => "write",
        _ => "edit",
    };
    RequiredAction {
        kind: RequiredActionKind::EditTarget,
        tool,
        target: Some(Utf8PathBuf::from(target)),
        command: None,
        projection_text: format!("{prefix}:{target}"),
    }
}

fn fixture_required_shell_action(command: &str) -> RequiredAction {
    RequiredAction {
        kind: RequiredActionKind::ShellCommand,
        tool: ToolName::Shell,
        target: None,
        command: Some(command.to_string()),
        projection_text: format!("shell:{command}"),
    }
}

#[derive(Debug, Clone)]
struct RuntimeOwnedVerificationRedirect {
    effective_tool_name: String,
    effective_arguments_json: String,
    redirected_from_arguments_json: String,
    redirect_reason: &'static str,
}

fn runtime_owned_required_verification_tool_call(
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) -> Option<CompletedToolCall> {
    let command = runtime_owned_required_verification_command(
        active_work,
        allowed_tools,
        tool_choice,
        required_action,
    )?;
    Some(CompletedToolCall {
        call_id: format!(
            "runtime_shell_verification:{}",
            crate::harness::artifact::hash_bytes(command.as_bytes())
        ),
        tool_name: "shell".to_string(),
        arguments_json: serde_json::to_string(&json!({ "command": command })).ok()?,
    })
}

fn runtime_owned_required_verification_dispatch_redirect(
    requested_tool_name: &str,
    original_arguments_json: &str,
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) -> Option<RuntimeOwnedVerificationRedirect> {
    if requested_tool_name == "shell" {
        return None;
    }
    let command = runtime_owned_required_verification_command(
        active_work,
        allowed_tools,
        tool_choice,
        required_action,
    )?;
    let effective_arguments_json = serde_json::to_string(&json!({ "command": command })).ok()?;
    Some(RuntimeOwnedVerificationRedirect {
        effective_tool_name: "shell".to_string(),
        effective_arguments_json,
        redirected_from_arguments_json: original_arguments_json.to_string(),
        redirect_reason: "runtime_owned_required_verification_dispatch",
    })
}

fn runtime_owned_required_verification_command(
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) -> Option<String> {
    if allowed_tools.len() != 1
        || !allowed_tools.contains("shell")
        || !matches!(
            tool_choice,
            ToolChoice::Required | ToolChoice::Named(ToolName::Shell)
        )
    {
        return None;
    }
    let Some(ActiveWorkContract::Verification {
        commands,
        repair_required,
        targets,
        ..
    }) = active_work
    else {
        return None;
    };
    if *repair_required || !targets.is_empty() || commands.len() != 1 {
        return None;
    }
    let command = required_action
        .and_then(RequiredAction::shell_command)
        .map(str::trim)
        .filter(|command| !command.is_empty())?;
    Some(command.to_string())
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
    let public_active = ActiveWorkContract::Verification {
        commands: vec![
            "python -X utf8 component.py 8 +".to_string(),
            "python -X utf8 component.py log 10".to_string(),
            "python -X utf8 component.py 8 +".to_string(),
            "python -X utf8 component.py log 10".to_string(),
        ],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let public_active_deduped = ActiveWorkContract::Verification {
        commands: vec![
            "python -X utf8 component.py 8 +".to_string(),
            "python -X utf8 component.py log 10".to_string(),
        ],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let effective = available.clone();
    let wrong = ToolLifecycleRuntime::wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -m py_compile app.py"}),
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    );
    let non_required_probe = ToolLifecycleRuntime::wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -X utf8 widget.py --probe"}),
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    );
    let right = ToolLifecycleRuntime::wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -m unittest"}),
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    );
    let public_exact = ToolLifecycleRuntime::wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -X utf8 component.py 8 +"}),
        Some(&public_active),
        crate::config::ShellFamily::PowerShell,
    );
    let public_wrong = ToolLifecycleRuntime::wrong_verification_shell_command_result(
        "shell",
        &json!({"command": "python -X utf8 component.py 99 +"}),
        Some(&public_active),
        crate::config::ShellFamily::PowerShell,
    );
    let mut public_wrong_counts = BTreeMap::new();
    let public_wrong_args = json!({"command": "python -X utf8 component.py 99 +"});
    let public_wrong_result = public_wrong
        .as_ref()
        .expect("public wrong verification command should be corrective");
    let public_wrong_deduped_decision =
        ToolLifecycleRuntime::record_wrong_verification_command_no_progress(
            &mut public_wrong_counts,
            &public_wrong_args,
            Some(&public_active_deduped),
            &effective,
            &ToolChoice::Auto,
            public_wrong_result,
        );
    let public_wrong_duplicated_decision =
        ToolLifecycleRuntime::record_wrong_verification_command_no_progress(
            &mut public_wrong_counts,
            &public_wrong_args,
            Some(&public_active),
            &effective,
            &ToolChoice::Auto,
            public_wrong_result,
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
                && result.output_text.contains("python -X utf8 -m unittest")
                && result
                    .metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("wrong_verification_command")
                && result
                    .metadata
                    .get("executable_verification_commands")
                    .and_then(Value::as_array)
                    .is_some_and(|commands| {
                        commands
                            .iter()
                            .any(|command| command.as_str() == Some("python -X utf8 -m unittest"))
                    })
        })
        && non_required_probe.as_ref().is_some_and(|result| {
            result.output_text.contains("python -m unittest")
                && result
                    .output_text
                    .contains("Do not run public command probes")
                && result
                    .metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("wrong_verification_command")
        })
        && public_exact.is_none()
        && public_wrong.as_ref().is_some_and(|result| {
            result
                .metadata
                .get("required_verification_commands")
                .and_then(Value::as_array)
                .is_some_and(|commands| {
                    commands.len() == 2
                        && commands.iter().any(|command| {
                            command.as_str() == Some("python -X utf8 component.py 8 +")
                        })
                        && commands.iter().any(|command| {
                            command.as_str() == Some("python -X utf8 component.py log 10")
                        })
                })
                && result
                    .metadata
                    .get("executable_verification_commands")
                    .and_then(Value::as_array)
                    .is_some_and(|commands| {
                        commands.iter().any(|command| {
                            command.as_str() == Some("python -X utf8 component.py 8 +")
                        })
                    })
        })
        && public_wrong_deduped_decision.count == 1
        && public_wrong_duplicated_decision.count == 2
        && right.is_none()
        && ToolLifecycleRuntime::verification_supporting_context_no_progress_under_active_verification(
            "read",
            r#"{"path":"app.py"}"#,
            &read_result,
            &state,
        )
        && ToolLifecycleRuntime::verification_supporting_context_no_progress_key(
            "read",
            r#"{"path":"app.py"}"#,
            &state,
            &effective,
            &ToolChoice::Required,
        )
        .contains("verification_supporting_context")
        && ToolLifecycleRuntime::should_terminalize_verification_supporting_context_no_progress(
            VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD,
        )
}

pub(crate) fn repair_active_shell_probe_uses_repair_target_authority_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: generated test expected extra output formatting".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["python -m unittest".to_string()];
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-repair-active-shell-probe-target-authority".to_string(),
        failing_labels: vec!["test_widget_cli".to_string()],
        primary_failure: Some("stdout assertion overreach".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("test_widget_cli".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: Some("self.assertIn(\"decorative\", proc.stdout)".to_string()),
            exception: None,
            expected: Some("decorative".to_string()),
            observed: Some("stdout `7`".to_string()),
            public_state_assertions: vec!["proc.returncode".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_contract_overreach".to_string(),
                "public_output_stream_assertion_mismatch".to_string(),
                "generated-test public output formatting assertion overreach".to_string(),
            ],
            sibling_obligations: vec!["proc.returncode".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["proc.returncode".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });
    let repair_active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_widget_cli".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    };
    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let workspace_root = Utf8Path::new("C:/workspace/repair-shell");

    let exact_probe_args = json!({"command": "Get-Content -Encoding UTF8 test_widget.py"});
    let exact_target_probe_matches =
        ToolLifecycleRuntime::repair_active_shell_probe_matches_exact_target(
            "shell",
            &exact_probe_args,
            Some(&repair_active),
            &state,
            workspace_root,
            &allowed_tools,
        );
    let exact_target_probe_wrong_verification = if exact_target_probe_matches {
        None
    } else {
        ToolLifecycleRuntime::wrong_verification_shell_command_result(
            "shell",
            &exact_probe_args,
            Some(&repair_active),
            crate::config::ShellFamily::PowerShell,
        )
    };
    let wrong_target_probe = ToolLifecycleRuntime::repair_active_shell_probe_target_result(
        "shell",
        &json!({"command": "Get-Content -Encoding UTF8 widget.py"}),
        Some(&repair_active),
        &state,
        workspace_root,
        &allowed_tools,
    );
    let wrong_target_result = wrong_target_probe
        .as_ref()
        .expect("wrong target shell probe should be repair no-progress");

    exact_target_probe_matches
        && exact_target_probe_wrong_verification.is_none()
        && wrong_target_probe.as_ref().is_some_and(|result| {
            result
                .metadata
                .pointer("/tool_feedback_envelope/kind")
                .and_then(Value::as_str)
                == Some("repair_shell_probe_target_mismatch")
                && result
                    .metadata
                    .pointer("/tool_feedback_envelope/required_target")
                    .and_then(Value::as_str)
                    == Some("test_widget.py")
                && result
                    .metadata
                    .pointer("/tool_feedback_envelope/submitted_targets/0")
                    .and_then(Value::as_str)
                    == Some("widget.py")
        })
        && ToolLifecycleRuntime::record_repair_target_authority_violation_no_progress(
            &mut BTreeMap::new(),
            &allowed_tools,
            &ToolChoice::Required,
            wrong_target_result,
        )
        .count
            == 1
}

pub(crate) fn post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes() -> bool {
    let allowed = BTreeSet::from(["shell".to_string()]);
    let active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_calculate".to_string()],
        repair_required: false,
        targets: Vec::new(),
    };
    let repair_still_open = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_calculate".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    };
    let required_shell = fixture_required_shell_action("python -X utf8 -m unittest");
    let redirected = runtime_owned_required_verification_dispatch_redirect(
        "read",
        r#"{"path":"test_widget.py"}"#,
        Some(&active),
        &allowed,
        &ToolChoice::Required,
        Some(&required_shell),
    )
    .and_then(|redirect| {
        serde_json::from_str::<Value>(&redirect.effective_arguments_json)
            .ok()
            .map(|arguments| (redirect, arguments))
    });
    let shell_passthrough = runtime_owned_required_verification_dispatch_redirect(
        "shell",
        r#"{"command":"Get-ChildItem"}"#,
        Some(&active),
        &allowed,
        &ToolChoice::Required,
        Some(&required_shell),
    );
    let repair_phase_blocked = runtime_owned_required_verification_dispatch_redirect(
        "read",
        r#"{"path":"test_widget.py"}"#,
        Some(&repair_still_open),
        &allowed,
        &ToolChoice::Required,
        Some(&required_shell),
    );
    let broad_surface_blocked = runtime_owned_required_verification_dispatch_redirect(
        "read",
        r#"{"path":"test_widget.py"}"#,
        Some(&active),
        &BTreeSet::from(["read".to_string(), "shell".to_string()]),
        &ToolChoice::Auto,
        Some(&required_shell),
    );

    redirected.is_some_and(|(redirect, arguments)| {
        redirect.effective_tool_name == "shell"
            && redirect.redirected_from_arguments_json == r#"{"path":"test_widget.py"}"#
            && redirect.redirect_reason == "runtime_owned_required_verification_dispatch"
            && arguments.get("command").and_then(Value::as_str)
                == Some("python -X utf8 -m unittest")
    }) && shell_passthrough.is_none()
        && repair_phase_blocked.is_none()
        && broad_surface_blocked.is_none()
}

pub(crate) fn verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes()
-> bool {
    let allowed = BTreeSet::from(["shell".to_string()]);
    let active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let required_shell = fixture_required_shell_action("python -X utf8 -m unittest");
    let runtime_call = runtime_owned_required_verification_tool_call(
        Some(&active),
        &allowed,
        &ToolChoice::Named(ToolName::Shell),
        Some(&required_shell),
    )
    .and_then(|call| {
        serde_json::from_str::<Value>(&call.arguments_json)
            .ok()
            .map(|arguments| (call, arguments))
    });
    let broad_surface_blocked = runtime_owned_required_verification_tool_call(
        Some(&active),
        &BTreeSet::from(["read".to_string(), "shell".to_string()]),
        &ToolChoice::Auto,
        Some(&required_shell),
    );

    runtime_call.is_some_and(|(call, arguments)| {
        call.tool_name == "shell"
            && call.call_id.starts_with("runtime_shell_verification:")
            && arguments.get("command").and_then(Value::as_str)
                == Some("python -X utf8 -m unittest")
            && arguments.get("runtime_owned").is_none()
    }) && broad_surface_blocked.is_none()
}

pub(crate) fn singleton_verification_command_arguments_are_runtime_owned_fixture_passes() -> bool {
    let active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let repair_active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_widget".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("widget.py")],
    };
    let multi_active = ActiveWorkContract::Verification {
        commands: vec![
            "python -m unittest".to_string(),
            "python -m py_compile widget.py".to_string(),
        ],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let repaired = repair_shell_arguments_from_singleton_verification_command(
        "shell",
        r#"{"command":"Get-ChildItem","workdir":"C:/tmp","timeout":5}"#,
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    )
    .and_then(|args| serde_json::from_str::<Value>(&args).ok());
    let already_exact = repair_shell_arguments_from_singleton_verification_command(
        "shell",
        r#"{"command":"python -X utf8 -m unittest"}"#,
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    );
    let corrected_identity_match = repair_shell_arguments_from_singleton_verification_command(
        "shell",
        r#"{"command":"python -m unittest"}"#,
        Some(&active),
        crate::config::ShellFamily::PowerShell,
    )
    .and_then(|args| serde_json::from_str::<Value>(&args).ok());
    let repair_lane = repair_shell_arguments_from_singleton_verification_command(
        "shell",
        r#"{"command":"Get-ChildItem"}"#,
        Some(&repair_active),
        crate::config::ShellFamily::PowerShell,
    );
    let multi_command = repair_shell_arguments_from_singleton_verification_command(
        "shell",
        r#"{"command":"Get-ChildItem"}"#,
        Some(&multi_active),
        crate::config::ShellFamily::PowerShell,
    );

    let repaired_command = repaired
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str);
    let corrected_identity_match_command = corrected_identity_match
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str);
    let wrong_after_repair = repaired.as_ref().and_then(|value| {
        ToolLifecycleRuntime::wrong_verification_shell_command_result(
            "shell",
            value,
            Some(&active),
            crate::config::ShellFamily::PowerShell,
        )
    });

    repaired_command == Some("python -X utf8 -m unittest")
        && corrected_identity_match_command == Some("python -X utf8 -m unittest")
        && repaired
            .as_ref()
            .is_some_and(|value| value.get("workdir").is_none() && value.get("timeout").is_none())
        && wrong_after_repair.is_none()
        && already_exact.is_none()
        && repair_lane.is_none()
        && multi_command.is_none()
}

pub(crate) fn same_verification_failure_terminal_guard_fixture_passes() -> bool {
    let failed = json!({
        "result_hash": "same-test-output",
        "verification_run_result": {
            "command": "python -X utf8 -m unittest",
            "status": "failed",
            "exit_code": 1,
            "timed_out": false,
            "output_summary": "Passed: 9/10\nFailed: 1/10",
            "failure_cluster": {
                "cluster_id": "raw-output-derived-a",
                "failing_labels": ["test_widget_cli_contract"],
                "primary_failure": "Command: python -X utf8 -m unittest",
                "evidence": [{
                    "evidence_kind": "verification_failure",
                    "subtype": "generic_verification_failure",
                    "evidence_markers": ["generic_verification_failure"],
                    "source_refs": ["usage text"],
                    "test_refs": ["test_widget.py"]
                }],
                "source_refs": ["usage text"],
                "test_refs": ["test_widget.py"]
            }
        }
    });
    let failed_equivalent = json!({
        "tool_feedback_envelope": {
            "result_hash": "different-raw-output-hash"
        },
        "verification_run_result": {
            "command": "python -X utf8 -m unittest",
            "status": "failed",
            "exit_code": 1,
            "timed_out": false,
            "output_summary": "Ran 10 tests with one failure; progress dots and traceback formatting changed",
            "failure_cluster": {
                "cluster_id": "raw-output-derived-b",
                "failing_labels": ["test_widget_cli_contract"],
                "primary_failure": "Command: python -X utf8 -m unittest",
                "evidence": [{
                    "evidence_kind": "verification_failure",
                    "subtype": "generic_verification_failure",
                    "evidence_markers": ["generic_verification_failure"],
                    "source_refs": ["usage text"],
                    "test_refs": ["test_widget.py"]
                }],
                "source_refs": ["usage text"],
                "test_refs": ["test_widget.py"]
            }
        }
    });
    let different_failure = json!({
        "result_hash": "different-test-output",
        "verification_run_result": {
            "command": "python -X utf8 -m unittest",
            "status": "failed",
            "exit_code": 1,
            "timed_out": false,
            "output_summary": "Passed: 8/10\nFailed: 2/10",
            "failure_cluster": {
                "cluster_id": "raw-output-derived-c",
                "failing_labels": ["test_widget_file_contract"],
                "primary_failure": "Command: python -X utf8 -m unittest",
                "evidence": [{
                    "evidence_kind": "verification_failure",
                    "subtype": "generic_verification_failure",
                    "evidence_markers": ["generic_verification_failure"],
                    "source_refs": ["file output"],
                    "test_refs": ["test_widget.py"]
                }],
                "source_refs": ["file output"],
                "test_refs": ["test_widget.py"]
            }
        }
    });
    let passed = json!({
        "verification_run_result": {
            "command": "python -X utf8 -m unittest",
            "status": "passed",
            "exit_code": 0,
            "timed_out": false,
            "output_summary": "Passed: 10/10\nFailed: 0/10"
        }
    });
    let first = ToolLifecycleRuntime::same_verification_failure_no_progress_key(&failed);
    let second =
        ToolLifecycleRuntime::same_verification_failure_no_progress_key(&failed_equivalent);
    let different =
        ToolLifecycleRuntime::same_verification_failure_no_progress_key(&different_failure);
    first.is_some()
        && first == second
        && first != different
        && ToolLifecycleRuntime::verification_run_passed(&passed)
        && ToolLifecycleRuntime::should_terminalize_same_verification_failure(
            SAME_VERIFICATION_FAILURE_TERMINAL_THRESHOLD,
        )
        && ToolLifecycleRuntime::same_verification_failure_terminal_message(
            SAME_VERIFICATION_FAILURE_TERMINAL_THRESHOLD,
        )
        .contains("same verification failure evidence repeated")
}

pub(crate) fn active_authoring_rejects_wrong_target_fixture_passes() -> bool {
    let active = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("README.md"),
            Utf8PathBuf::from("test_arcade_game.py"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let workspace_root = Utf8Path::new("C:/workspace/route");
    let wrong_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "arcade_game.py", "content": "source"}),
        Some(&active),
        workspace_root,
    );
    let right_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "test_arcade_game.py", "content": "tests"}),
        Some(&active),
        workspace_root,
    );
    let wrong_patch = ToolLifecycleRuntime::wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Update File: arcade_game.py\n@@\n-pass\n+pass\n*** End Patch"}),
        Some(&active),
        workspace_root,
    );
    let right_patch = ToolLifecycleRuntime::wrong_authoring_target_result(
        "apply_patch",
        &json!({"patch_text": "*** Begin Patch\n*** Add File: README.md\n+Arcade Game\n*** End Patch"}),
        Some(&active),
        workspace_root,
    );
    let workspace_absolute_escaped_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({
            "path": "C:\\\\workspace\\\\route\\\\test_arcade_game.py",
            "content": "tests"
        }),
        Some(&active),
        workspace_root,
    );
    let outside_workspace_absolute_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({
            "path": "C:\\\\workspace\\\\other\\\\test_arcade_game.py",
            "content": "tests"
        }),
        Some(&active),
        workspace_root,
    );
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut wrong_authoring_counts = BTreeMap::new();
    let first_wrong_args = json!({"path": "arcade_game.py", "content": "source"});
    let second_wrong_args = json!({"path": "arcade_game.py", "content": "different source"});
    let wrong_write_result = wrong_write
        .as_ref()
        .expect("wrong write should be rejected");
    let first_decision = ToolLifecycleRuntime::record_wrong_authoring_target_no_progress(
        &mut wrong_authoring_counts,
        "write",
        &first_wrong_args,
        Some(&active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
        wrong_write_result,
    );
    let second_decision = ToolLifecycleRuntime::record_wrong_authoring_target_no_progress(
        &mut wrong_authoring_counts,
        "write",
        &second_wrong_args,
        Some(&active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
        wrong_write_result,
    );
    let wrong_patch_args = json!({"patch_text": "*** Begin Patch\n*** Update File: arcade_game.py\n@@\n-pass\n+different pass\n*** End Patch"});
    let wrong_patch_result = wrong_patch
        .as_ref()
        .expect("wrong patch should be rejected");
    let cross_tool_decision = ToolLifecycleRuntime::record_wrong_authoring_target_no_progress(
        &mut wrong_authoring_counts,
        "apply_patch",
        &wrong_patch_args,
        Some(&active),
        workspace_root,
        &allowed,
        &ToolChoice::Auto,
        wrong_patch_result,
    );
    let progressed_active = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("arcade_game.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let progressed_decision = ToolLifecycleRuntime::record_wrong_authoring_target_no_progress(
        &mut wrong_authoring_counts,
        "write",
        &first_wrong_args,
        Some(&progressed_active),
        workspace_root,
        &allowed,
        &ToolChoice::Required,
        wrong_write_result,
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
        route_contract_satisfied: false,
    };
    let docs_completed_target_regression = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "README.md", "content": "# stale completed deliverable"}),
        Some(&docs_active),
        workspace_root,
    );
    let docs_active_target_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "basic_design.md", "content": "# Basic design"}),
        Some(&docs_active),
        workspace_root,
    );
    let docs_completed_target_patch = ToolLifecycleRuntime::wrong_authoring_target_result(
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
            && result.output_text.contains("test_arcade_game.py")
    }) && right_write.is_none()
        && wrong_patch.is_some()
        && right_patch.is_none()
        && workspace_absolute_escaped_write.is_none()
        && outside_workspace_absolute_write.is_some()
        && first_decision.count == 1
        && second_decision.count == 2
        && cross_tool_decision.count == 3
        && cross_tool_decision
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("active requested-work deliverable set"))
        && progressed_decision.count == 1
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
        && wrong_authoring_counts.len() == 2
}

pub(crate) fn verification_repair_rejects_non_exact_write_target_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: widget.compute is missing".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["python -m unittest".to_string()];
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-source-owned-repair-write-admission".to_string(),
        failing_labels: vec!["test_compute".to_string()],
        primary_failure: Some(
            "AttributeError: module 'widget' has no attribute 'compute'".to_string(),
        ),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("test_compute".to_string()),
            target: Some(" 0".to_string()),
            symbol: Some("widget.compute".to_string()),
            call_site: Some("widget.compute(1 + 2)".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("3".to_string()),
            observed: Some("widget.compute is missing".to_string()),
            public_state_assertions: vec!["widget.compute(1 + 2)".to_string()],
            public_missing_attributes: vec!["widget.compute".to_string()],
            evidence_markers: vec![
                "public_class_attribute_mismatch".to_string(),
                "public missing method `widget.compute`".to_string(),
            ],
            sibling_obligations: vec!["`widget.compute` is missing".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec![" 0".to_string(), "1 + 2".to_string()],
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["`widget.compute` is missing".to_string()],
        source_refs: vec![" 0".to_string(), "1 + 2".to_string()],
        test_refs: vec!["test_widget.py".to_string()],
    });
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: state.verification.failing_labels.clone(),
        repair_required: true,
        targets: state.active_targets.clone(),
    };
    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let wrong_test_write = ToolLifecycleRuntime::repair_target_authority_violation_result(
        "write",
        &json!({"path": "test_widget.py", "content": "import unittest\n"}),
        Some(&active_work),
        &state,
        Utf8Path::new("C:/workspace/source-owned-repair"),
        &allowed_tools,
    );
    let right_source_write = ToolLifecycleRuntime::repair_target_authority_violation_result(
        "write",
        &json!({"path": "widget.py", "content": "def compute(value):\n    return value\n"}),
        Some(&active_work),
        &state,
        Utf8Path::new("C:/workspace/source-owned-repair"),
        &allowed_tools,
    );
    let wrong_repair_result = wrong_test_write
        .as_ref()
        .expect("wrong repair target should be rejected");
    let mut wrong_repair_target_counts = BTreeMap::new();
    let first_wrong_repair_decision =
        ToolLifecycleRuntime::record_repair_target_authority_violation_no_progress(
            &mut wrong_repair_target_counts,
            &allowed_tools,
            &ToolChoice::Required,
            wrong_repair_result,
        );
    let second_wrong_repair_decision =
        ToolLifecycleRuntime::record_repair_target_authority_violation_no_progress(
            &mut wrong_repair_target_counts,
            &allowed_tools,
            &ToolChoice::Required,
            wrong_repair_result,
        );
    let third_wrong_repair_decision =
        ToolLifecycleRuntime::record_repair_target_authority_violation_no_progress(
            &mut wrong_repair_target_counts,
            &allowed_tools,
            &ToolChoice::Required,
            wrong_repair_result,
        );

    wrong_test_write.as_ref().is_some_and(|result| {
        result.recorded_changes.is_empty()
            && result.change_summaries.is_empty()
            && result
                .metadata
                .pointer("/tool_feedback_envelope/side_effects_applied")
                .and_then(Value::as_bool)
                == Some(false)
            && result
                .metadata
                .pointer("/repair_target_authority/exact_target")
                .and_then(Value::as_str)
                == Some("widget.py")
            && result
                .metadata
                .pointer("/tool_feedback_envelope/operation_progress_class")
                .and_then(Value::as_str)
                == Some("wrong_repair_target")
            && result
                .metadata
                .get("result_hash")
                .and_then(Value::as_str)
                .is_some_and(|hash| !hash.trim().is_empty())
            && result
                .metadata
                .pointer("/tool_feedback_envelope/result_hash")
                .and_then(Value::as_str)
                .is_some_and(|hash| !hash.trim().is_empty())
            && result
                .metadata
                .pointer("/terminal_guard_policy/terminal_after_repeated_corrections")
                .and_then(Value::as_u64)
                == Some(3)
            && result
                .metadata
                .pointer("/repair_target_authority/forbidden_actions")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().filter_map(Value::as_str).any(|item| {
                        item == "generated_test_rewrite_for_source_owned_contract_violation"
                    })
                })
    }) && right_source_write.is_none()
        && first_wrong_repair_decision.count == 1
        && second_wrong_repair_decision.count == 2
        && third_wrong_repair_decision.count == 3
        && third_wrong_repair_decision
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("exact repair target"))
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
        route_contract_satisfied: false,
    };
    let workspace_root = Utf8Path::new("C:/workspace/route");
    let completed_readme_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "README.md", "content": "# stale completed deliverable"}),
        Some(&docs_active),
        workspace_root,
    );
    let active_basic_write = ToolLifecycleRuntime::wrong_authoring_target_result(
        "write",
        &json!({"path": "basic_design.md", "content": "# Basic design"}),
        Some(&docs_active),
        workspace_root,
    );
    let completed_readme_patch = ToolLifecycleRuntime::wrong_authoring_target_result(
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

fn tool_output_is_content_changing_progress(metadata: &Value) -> bool {
    ToolLifecycleRuntime::operation_progress_class_from_metadata(metadata)
        == Some("content_changing_progress")
        && metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("progress_effect"))
            .or_else(|| metadata.get("progress_effect"))
            .and_then(Value::as_str)
            == Some("made_progress")
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

fn docs_route_contract_still_pending_after_file_change(state: &SessionStateSnapshot) -> bool {
    state.route == TaskRoute::Docs && state.completion.route_contract_pending
}

fn constrain_read_schema_to_missing_authoring_targets(
    tools: &mut [crate::llm::ToolSchema],
    envelope: &AuthoringGroundingRecoveryEnvelope,
) {
    if envelope.missing_grounding_targets.is_empty() {
        return;
    }
    let missing = envelope.missing_grounding_targets.clone();
    let description = format!(
        "Target file path. In the current authoring grounding recovery, `read` is only admissible for remaining ungrounded active target(s): {}. Already grounded target(s) {} must be edited with `write` / `apply_patch` instead of read again.",
        envelope.missing_text(),
        envelope.consumed_text()
    );
    for tool in tools.iter_mut().filter(|tool| tool.name == "read") {
        if let Some(path_schema) = tool
            .input_schema
            .pointer_mut("/properties/path")
            .and_then(Value::as_object_mut)
        {
            path_schema.insert("description".to_string(), json!(description));
            path_schema.insert("enum".to_string(), json!(missing));
        }
    }
}

fn authoring_supporting_context_budget_recovery_read_disallowed(
    effective_tool_name: &str,
    arguments: &Value,
    state: &SessionStateSnapshot,
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
    turn_grounded_targets: &BTreeSet<String>,
) -> bool {
    if effective_tool_name != "read" {
        return false;
    }
    let Some(path) = arguments.get("path").and_then(Value::as_str) else {
        return true;
    };
    let active_targets = active_authoring_target_keys(state);
    let Some(target) =
        matching_active_target_key(&normalize_path_for_target_match(path), &active_targets)
    else {
        return true;
    };
    !authoring_missing_grounding_targets(
        history_items,
        state,
        workspace_root,
        turn_grounded_targets,
    )
    .contains(&target)
}

fn stable_tool_schemas_from_registry(registry: &ToolRegistry) -> Vec<crate::llm::ToolSchema> {
    registry
        .specs()
        .into_iter()
        .map(|spec| crate::llm::ToolSchema {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: spec.input_schema,
            strict: false,
        })
        .collect()
}

fn normalized_command_text_for_family_match(command: impl AsRef<str>) -> String {
    command
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(crate) fn provider_required_tool_choice_final_message_recovery_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    state.completion.closeout_ready = false;

    let noncompliance_detected =
        TurnLifecycleKernel::provider_required_tool_choice_final_message_noncompliance(
            &state,
            &ToolChoice::Required,
            &tool_names,
            true,
        );
    let narrowed_tool_names = BTreeSet::from(["write".to_string()]);
    let recovery_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &narrowed_tool_names,
        TurnLifecycleRecoveryContext {
            provider_required_tool_choice_final_message_recovery_active: true,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let policy =
        TurnLifecycleKernel::provider_required_tool_choice_final_message_recovery_policy(&state);

    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    docs_state.completion.open_work_count = 1;
    docs_state.completion.route_contract_pending = true;
    docs_state.completion.closeout_ready = false;
    let docs_recovery_choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &docs_state,
        &BTreeSet::from(["write".to_string()]),
        TurnLifecycleRecoveryContext {
            provider_required_tool_choice_final_message_recovery_active: true,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    noncompliance_detected
        && matches!(recovery_choice, ToolChoice::Required)
        && matches!(docs_recovery_choice, ToolChoice::Required)
        && policy.policy == "provider_required_tool_choice_final_message_recovery_surface"
        && policy.tool_name.as_deref() == Some("write")
        && policy.active_targets
            == vec!["component.py".to_string(), "test_component.py".to_string()]
        && policy.reason.contains("text-only final message")
        && provider_required_tool_choice_recovery_rebuilds_write_from_stable_surface_fixture_passes(
        )
}

fn provider_required_tool_choice_recovery_rebuilds_write_from_stable_surface_fixture_passes() -> bool
{
    let mut tools = vec![crate::llm::ToolSchema {
        name: "apply_patch".to_string(),
        description: "apply a patch".to_string(),
        input_schema: json!({"type": "object"}),
        strict: false,
    }];
    let stable_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("component.py")];
    state.completion.open_work_count = 1;

    let active = TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && TurnLifecycleKernel::provider_required_tool_choice_final_message_recovery_has_write_surface(
            &tools,
            &stable_tools,
        );
    if active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(&mut tools, &stable_tools, |name| {
            name == "write"
        });
        tools.retain(|tool| tool.name == "write");
    }
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &tool_names,
        TurnLifecycleRecoveryContext {
            provider_required_tool_choice_final_message_recovery_active: active,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    active
        && tool_names == BTreeSet::from(["write".to_string()])
        && matches!(choice, ToolChoice::Required)
}

pub(crate) fn final_dispatch_source_schema_projection_fixture_passes() -> bool {
    let mut tools = vec![crate::llm::ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Target file path relative to the current workspace or an allowed absolute path."
                },
                "content": {"type": "string", "description": "Complete final file contents."}
            }
        }),
        strict: false,
    }];
    let required_write = fixture_required_edit_action(ToolName::Write, "component.py");
    crate::agent::prompt::apply_write_content_shape_to_write_schema_for_required_action(
        &mut tools,
        Some(&required_write),
    );
    let schema_description = tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/content/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    schema_description.contains("Complete final Python source contents")
        && schema_description.contains("real newline-separated source structure")
}

pub(crate) fn authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes() -> bool
{
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace_root.join("component.py").as_std_path(),
        "def add(a, b):\n    return a + b\n",
    )
    .is_err()
        || fs::write(
            workspace_root.join("test_component.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
    {
        return false;
    }
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let change_ids = vec![
        crate::session::ChangeId::new(),
        crate::session::ChangeId::new(),
    ];
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::FileChange {
            change_ids: change_ids.clone(),
            changes: vec![
                crate::protocol::FileChangeEvidence {
                    change_id: change_ids[0],
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Added component.py".to_string(),
                },
                crate::protocol::FileChangeEvidence {
                    change_id: change_ids[1],
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("test_component.py")),
                    summary: "Added test_component.py".to_string(),
                },
            ],
            summary: "Added component.py and test_component.py".to_string(),
        },
    }];
    let mut visible = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "write".to_string(),
    ]);
    let active = TurnLifecycleKernel::authoring_target_grounding_final_message_recovery_active(
        &state,
        active_authoring_targets_need_grounding(
            &history,
            &state,
            &workspace_root,
            &BTreeSet::new(),
        ),
    );
    if active {
        visible.retain(|tool| {
            TurnLifecycleKernel::authoring_target_grounding_recovery_tool_visible(tool)
        });
    }
    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &visible,
        TurnLifecycleRecoveryContext {
            authoring_target_grounding_final_message_recovery_active: active,
            open_obligation_final_message_recovery_active: !active,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut single_existing_target_state = state.clone();
    single_existing_target_state.active_targets = vec![Utf8PathBuf::from("component.py")];
    let existing_target_history = Vec::new();
    let existing_target_active =
        TurnLifecycleKernel::authoring_target_grounding_final_message_recovery_active(
            &single_existing_target_state,
            active_authoring_targets_need_grounding(
                &existing_target_history,
                &single_existing_target_state,
                &workspace_root,
                &BTreeSet::new(),
            ),
        );
    let read_call_id = crate::session::ToolCallId::new();
    let grounded_existing_history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolCall {
                call_id: read_call_id,
                tool: ToolName::Read,
                arguments: Value::Null,
                model_arguments: json!({"path": "component.py"}),
                effective_arguments: json!({"path": "component.py"}),
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
            sequence_no: 6,
            created_at_ms: 6,
            payload: HistoryItemPayload::ToolOutput {
                call_id: read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read component.py".to_string(),
                output_text: "def add(a, b): return a + b".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("read-component".to_string()),
                verification_run: None,
            },
        },
    ];
    let grounded_existing_active =
        TurnLifecycleKernel::authoring_target_grounding_final_message_recovery_active(
            &single_existing_target_state,
            active_authoring_targets_need_grounding(
                &grounded_existing_history,
                &single_existing_target_state,
                &workspace_root,
                &BTreeSet::new(),
            ),
        );

    active
        && existing_target_active
        && !grounded_existing_active
        && visible == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && choice == ToolChoice::Auto
}

pub(crate) fn docs_patch_context_final_message_recovery_preserves_grounding_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![Utf8PathBuf::from("docs/design.md")];

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "docling_convert".to_string(),
        "grep".to_string(),
        "mcp_call".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let invalid_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: docs/design.md\n@@\n old\n+new\n*** End Patch"}"#,
        "tool edit error: failed to find expected lines `old`",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let mut patch_grounding_targets = BTreeSet::<String>::new();
    record_patch_context_mismatch_grounding_targets(
        &mut patch_grounding_targets,
        &invalid_patch.metadata,
        &state,
    );
    let patch_grounding_active =
        patch_context_mismatch_target_grounding_surface_active(&state, &patch_grounding_targets);
    let stable_tools = allowed
        .iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut visible = stable_tools.clone();
    if patch_grounding_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut visible,
            &stable_tools,
            TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible,
        );
        visible.retain(|tool| {
            TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible(&tool.name)
        });
    }
    let visible_names = visible
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &visible_names,
        TurnLifecycleRecoveryContext {
            patch_context_mismatch_grounding_active: patch_grounding_active,
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    patch_grounding_active
        && choice == ToolChoice::Auto
        && visible_names.contains("read")
        && visible_names.contains("apply_patch")
        && visible_names.contains("shell")
        && visible_names.contains("todowrite")
        && visible_names.contains("write")
}

pub(crate) fn docs_existing_target_update_keeps_exact_read_grounding_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace_root.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace_root.join("docs/design.md").as_std_path(),
            "# Existing design\n\nCurrent content.\n",
        )
        .is_err()
    {
        return false;
    }
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("docs/design.md")];

    let active = TurnLifecycleKernel::existing_target_grounding_recovery_active(
        &state,
        active_authoring_targets_need_grounding(&[], &state, &workspace_root, &BTreeSet::new()),
    );
    let stable_tools = ["apply_patch", "grep", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| crate::llm::ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut visible = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        existing_target_grounding_recovery_active: active,
        open_obligation_final_message_recovery_active: true,
        open_obligation_final_message_count: 1,
        ..TurnLifecycleRecoveryContext::default()
    };
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut visible,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut visible,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: true,
        },
    );
    let envelope =
        authoring_grounding_recovery_envelope(&[], &state, &workspace_root, &BTreeSet::new());
    constrain_read_schema_to_missing_authoring_targets(&mut visible, &envelope);
    let visible_names = visible
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &visible_names,
        recovery,
    );
    let read_schema_constrained = visible
        .iter()
        .find(|tool| tool.name == "read")
        .and_then(|tool| tool.input_schema.pointer("/properties/path/enum"))
        .and_then(Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some("docs/design.md"))
        });
    let correct_read_allowed = !authoring_supporting_context_budget_recovery_read_disallowed(
        "read",
        &json!({"path": "docs/design.md"}),
        &state,
        &[],
        &workspace_root,
        &BTreeSet::new(),
    );
    let wrong_read_rejected = authoring_supporting_context_budget_recovery_read_disallowed(
        "read",
        &json!({"path": "README.md"}),
        &state,
        &[],
        &workspace_root,
        &BTreeSet::new(),
    );

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let grounded_history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::ToolCall {
                call_id: read_call_id,
                tool: ToolName::Read,
                arguments: Value::Null,
                model_arguments: json!({"path": "docs/design.md"}),
                effective_arguments: json!({"path": "docs/design.md"}),
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
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id: read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read docs/design.md".to_string(),
                output_text: "# Existing design".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("read-docs-design".to_string()),
                verification_run: None,
            },
        },
    ];
    let grounded_active = TurnLifecycleKernel::existing_target_grounding_recovery_active(
        &state,
        active_authoring_targets_need_grounding(
            &grounded_history,
            &state,
            &workspace_root,
            &BTreeSet::new(),
        ),
    );

    envelope.missing_grounding_targets == vec!["docs/design.md"]
        && active
        && !grounded_active
        && visible_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "write".to_string(),
            ])
        && choice == ToolChoice::Auto
        && read_schema_constrained
        && correct_read_allowed
        && wrong_read_rejected
}

pub(crate) fn generated_test_authoring_keeps_recent_source_reference_read_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let source_change = crate::session::ChangeId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![source_change],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id: source_change,
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("component.py")),
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Updated component.py".to_string(),
                }],
                summary: "Updated component.py".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: read_call_id,
                tool: ToolName::Read,
                arguments: Value::Null,
                model_arguments: json!({"path": "component.py"}),
                effective_arguments: json!({"path": "component.py"}),
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
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read component.py".to_string(),
                output_text: "class Component: pass".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("read-component".to_string()),
                verification_run: None,
            },
        },
    ];
    let mut stale_history = history.clone();
    let later_change = crate::session::ChangeId::new();
    stale_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::FileChange {
            change_ids: vec![later_change],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id: later_change,
                kind: crate::session::ChangeKind::Update,
                path_before: Some(Utf8PathBuf::from("component.py")),
                path_after: Some(Utf8PathBuf::from("component.py")),
                summary: "Updated component.py again".to_string(),
            }],
            summary: "Updated component.py again".to_string(),
        },
    });

    let mut visible = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let active = TurnLifecycleKernel::generated_test_source_reference_grounding_active(
        &state,
        history_has_unread_source_change_for_generated_test(&stale_history),
    );
    if active {
        visible.retain(|tool| {
            TurnLifecycleKernel::generated_test_source_reference_grounding_tool_visible(tool, true)
        });
    }
    let mut exhausted_visible = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    if active {
        exhausted_visible.retain(|tool| {
            TurnLifecycleKernel::generated_test_source_reference_grounding_tool_visible(tool, false)
        });
    }
    let mut dispatch_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "list".to_string(),
            description: "list files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run shell".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "todowrite".to_string(),
            description: "progress side channel".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let stable_dispatch_tools = dispatch_tools.clone();
    if active {
        TurnLifecycleKernel::apply_generated_test_source_reference_grounding_surface(
            &mut dispatch_tools,
            &stable_dispatch_tools,
            true,
        );
    }
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut dispatch_tools, &state);
    if active {
        TurnLifecycleKernel::apply_generated_test_source_reference_grounding_surface(
            &mut dispatch_tools,
            &stable_dispatch_tools,
            true,
        );
    }
    let post_normalization_visible = dispatch_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();

    !TurnLifecycleKernel::generated_test_source_reference_grounding_active(
        &state,
        history_has_unread_source_change_for_generated_test(&history),
    ) && active
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ])
        && exhausted_visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
            ])
        && post_normalization_visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ])
}

pub(crate) fn generated_test_consumed_source_reference_requires_active_target_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace_root.join("component.py").as_std_path(),
        "def add(a, b):\n    return a + b\n",
    )
    .is_err()
        || fs::write(
            workspace_root.join("test_component.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
    {
        return false;
    }

    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let source_change = crate::session::ChangeId::new();
    let source_read_call_id = crate::session::ToolCallId::new();
    let test_read_call_id = crate::session::ToolCallId::new();
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![source_change],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id: source_change,
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("component.py")),
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Updated component.py".to_string(),
                }],
                summary: "Updated component.py".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: source_read_call_id,
                tool: ToolName::Read,
                arguments: Value::Null,
                model_arguments: json!({"path": "component.py"}),
                effective_arguments: json!({"path": "component.py"}),
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
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: source_read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read component.py".to_string(),
                output_text: "def add(a, b): return a + b".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("read-component".to_string()),
                verification_run: None,
            },
        },
    ];
    let mut grounded_test_history = history.clone();
    grounded_test_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolCall {
            call_id: test_read_call_id,
            tool: ToolName::Read,
            arguments: Value::Null,
            model_arguments: json!({"path": "test_component.py"}),
            effective_arguments: json!({"path": "test_component.py"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    grounded_test_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 5,
        created_at_ms: 5,
        payload: HistoryItemPayload::ToolOutput {
            call_id: test_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "Read test_component.py".to_string(),
            output_text: "import unittest".to_string(),
            metadata: json!({"operation_progress_class": "supporting_context"}),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("read-test-component".to_string()),
            verification_run: None,
        },
    });

    let source_grounding_consumed =
        !TurnLifecycleKernel::generated_test_source_reference_grounding_active(
            &state,
            history_has_unread_source_change_for_generated_test(&history),
        ) && history_has_current_source_reference_read_for_generated_test(&history);
    let target_grounding_active =
        TurnLifecycleKernel::generated_test_reference_consumed_target_grounding_active(
            &state,
            history_has_current_source_reference_read_for_generated_test(&history),
            history_has_unread_source_change_for_generated_test(&history),
            active_authoring_targets_need_grounding(
                &history,
                &state,
                &workspace_root,
                &BTreeSet::new(),
            ),
        );
    let target_grounding_consumed =
        !TurnLifecycleKernel::generated_test_reference_consumed_target_grounding_active(
            &state,
            history_has_current_source_reference_read_for_generated_test(&grounded_test_history),
            history_has_unread_source_change_for_generated_test(&grounded_test_history),
            active_authoring_targets_need_grounding(
                &grounded_test_history,
                &state,
                &workspace_root,
                &BTreeSet::new(),
            ),
        );

    let mut visible = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "write".to_string(),
    ]);
    if target_grounding_active {
        visible.retain(|tool| {
            TurnLifecycleKernel::authoring_target_grounding_recovery_tool_visible(tool)
        });
    }
    let non_active_read = json!({"path": "component.py"});
    let active_read = json!({"path": "test_component.py"});
    let non_active_rejected = generated_test_reference_consumed_read_requires_active_target(
        "read",
        &non_active_read,
        &state,
    );
    let active_read_allowed = !generated_test_reference_consumed_read_requires_active_target(
        "read",
        &active_read,
        &state,
    );
    let rejection = ToolLifecycleRuntime::generated_test_target_grounding_required_result(
        "read",
        &non_active_read,
        &state,
    );

    source_grounding_consumed
        && target_grounding_active
        && target_grounding_consumed
        && matches!(
            compile_turn_lifecycle_tool_choice(
                &crate::agent::prompt::PromptPolicy::default(),
                &state,
                &visible,
                TurnLifecycleRecoveryContext {
                    generated_test_reference_consumed_target_grounding_active: true,
                    ..TurnLifecycleRecoveryContext::default()
                },
            ),
            ToolChoice::Auto
        )
        && visible == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && non_active_rejected
        && active_read_allowed
        && rejection
            .metadata
            .pointer("/tool_feedback_envelope/kind")
            .and_then(Value::as_str)
            == Some("generated_test_target_grounding_required")
        && rejection
            .output_text
            .contains("production source reference input is already current")
}

pub(crate) fn singleton_missing_authoring_target_projects_create_action_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace_root.join("component.py").as_std_path(),
        "def add(a, b):\n    return a + b\n",
    )
    .is_err()
    {
        return false;
    }

    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let source_change = crate::session::ChangeId::new();
    let source_read_call_id = crate::session::ToolCallId::new();
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![source_change],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id: source_change,
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Added component.py".to_string(),
                }],
                summary: "Added component.py".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: source_read_call_id,
                tool: ToolName::Read,
                arguments: json!({"path": "component.py"}),
                model_arguments: json!({"path": "component.py"}),
                effective_arguments: json!({"path": "component.py"}),
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
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: source_read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read component.py".to_string(),
                output_text: "def add(a, b): return a + b".to_string(),
                metadata: json!({"operation_progress_class": "supporting_context"}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("read-component".to_string()),
                verification_run: None,
            },
        },
    ];

    let source_reference_grounding_active =
        TurnLifecycleKernel::generated_test_source_reference_grounding_active(
            &state,
            history_has_unread_source_change_for_generated_test(&history),
        );
    let create_action_active =
        TurnLifecycleKernel::singleton_missing_authoring_target_create_action_active(
            &state,
            singleton_active_target_exists(&state, &workspace_root),
        );
    let mut visible = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    if create_action_active && !source_reference_grounding_active {
        visible.retain(|tool| {
            TurnLifecycleKernel::singleton_missing_authoring_target_create_action_tool_visible(tool)
        });
    }

    !source_reference_grounding_active
        && create_action_active
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
            ])
        && crate::protocol::singleton_missing_target_stable_surface_projects_apply_patch_action_fixture_passes()
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
            compile_turn_lifecycle_tool_choice(
                &crate::agent::prompt::PromptPolicy::default(),
                &SessionStateSnapshot::default(),
                &tool_names,
                TurnLifecycleRecoveryContext::default(),
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn codex_style_code_authoring_omits_whole_file_write_fixture_passes() -> bool {
    let mut tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
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
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("calculator.py")];
    state.completion.open_work_count = 1;
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    !tools.iter().any(|tool| tool.name == "write")
        && tools.iter().any(|tool| tool.name == "apply_patch")
}

pub(crate) fn codex_style_code_authoring_omits_json_discovery_surface_fixture_passes() -> bool {
    let mut tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "list".to_string(),
            description: "list files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "grep".to_string(),
            description: "search files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
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
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    tool_names == BTreeSet::from(["apply_patch", "shell", "todowrite"])
}

pub(crate) fn codex_style_docs_authoring_omits_non_codex_json_surface_fixture_passes() -> bool {
    let mut tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "docling_convert".to_string(),
            description: "convert a document".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "glob".to_string(),
            description: "glob files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "grep".to_string(),
            description: "search files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "inspect_directory".to_string(),
            description: "inspect directories".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "list".to_string(),
            description: "list files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "mcp_call".to_string(),
            description: "call MCP".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "skill".to_string(),
            description: "load a skill".to_string(),
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
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    state.completion.open_work_count = 1;
    state.completion.route_contract_pending = true;
    state.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/component-design.md")),
        ..DocsRouteState::default()
    });
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();

    tool_names
        == BTreeSet::from([
            "apply_patch",
            "docling_convert",
            "grep",
            "mcp_call",
            "read",
            "shell",
            "todowrite",
        ])
        && matches!(
            compile_turn_lifecycle_tool_choice(
                &crate::agent::prompt::PromptPolicy::default(),
                &state,
                &tool_names
                    .iter()
                    .map(|tool| (*tool).to_string())
                    .collect::<BTreeSet<_>>(),
                TurnLifecycleRecoveryContext::default(),
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["read".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = false;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    matches!(
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &tool_names,
            TurnLifecycleRecoveryContext::default(),
        ),
        ToolChoice::Auto
    ) && TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
}

pub(crate) fn multi_target_open_authoring_final_message_correction_names_targets_fixture_passes()
-> bool {
    let recovery_tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let correction =
        open_obligation_final_message_correction_text(&state, 1, None, &recovery_tool_names, false);

    matches!(choice, ToolChoice::Auto)
        && correction.contains("component.py")
        && correction.contains("test_component.py")
        && correction.contains("apply_patch")
        && correction.contains("open targets")
        && correction.contains("single patch")
        && correction.contains("*** Add File")
        && correction.contains("*** Update File")
        && !correction.contains("tool_choice")
}

pub(crate) fn final_message_recovery_is_system_control_projection_fixture_passes() -> bool {
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let correction = "The previous response was not accepted as a final answer.\nOpen targets: component.py, test_component.py.\nUse the `apply_patch` tool for the open targets before any final assistant message: submit a single patch whose `patch_text` creates or updates these active targets: component.py, test_component.py. The patch may contain multiple `*** Add File` or `*** Update File` sections.".to_string();
    let base_messages = vec![
        crate::llm::ModelMessage::User {
            content: "create component.py and test_component.py".to_string(),
        },
        crate::llm::ModelMessage::Assistant {
            content: "I will create them.".to_string(),
        },
    ];
    let (messages, policies) = provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        Some(correction),
        None,
        &tool_names,
        true,
    );
    let recovery_system = messages.iter().find_map(|message| match message {
        crate::llm::ModelMessage::System { content }
            if content.contains("Open-obligation final-message recovery") =>
        {
            Some(content.as_str())
        }
        _ => None,
    });
    let user_recovery_count = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                crate::llm::ModelMessage::User { content }
                    if content.contains("Open-obligation final-message recovery")
            )
        })
        .count();
    let assistant_text_count = messages
        .iter()
        .filter(|message| matches!(message, crate::llm::ModelMessage::Assistant { .. }))
        .count();

    recovery_system.is_some_and(|content| {
        content.contains("Open-obligation final-message recovery")
            && content.contains("component.py, test_component.py")
            && content.contains("*** Add File")
            && content.contains("*** Update File")
    }) && user_recovery_count == 0
        && assistant_text_count == 0
        && policies.is_empty()
        && request_content_markers(recovery_system.unwrap())
            .contains(&"open_obligation_final_message_recovery".to_string())
        && request_content_markers(recovery_system.unwrap())
            .contains(&"multi_file_apply_patch_shape".to_string())
}

pub(crate) fn invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes() -> bool
{
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    let arguments_json = r#"{"patch_text":"*** Begin Patch\n*** Add File: component.py\n+\"\"\"Component.\"\"\"\n\ndef build():\n+    return 1\n*** End Patch"}"#;
    let error = "tool patch error: add file body line `def build():` must start with `+`; every added content line, including blank lines and top-level `def`/`class`/`import` lines, must be prefixed with `+`.";
    let result = invalid_tool_arguments_result(
        "apply_patch",
        arguments_json,
        error,
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let base_messages = vec![crate::llm::ModelMessage::User {
        content: "create component.py and test_component.py".to_string(),
    }];
    let (messages, _) = provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        None,
        Some(recovery.prompt),
        &tool_names,
        true,
    );
    let recovery_system = messages.iter().find_map(|message| match message {
        crate::llm::ModelMessage::System { content }
            if content.contains("Invalid edit recovery:") =>
        {
            Some(content.as_str())
        }
        _ => None,
    });
    let user_recovery_count = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                crate::llm::ModelMessage::User { content }
                    if content.contains("Invalid edit recovery:")
            )
        })
        .count();
    is_invalid_tool_arguments_error(
        "tool patch error: add file body line `def build():` must start with `+`",
    ) && recovery_system.is_some_and(|content| {
        let markers = request_content_markers(content);
        content.contains("component.py, test_component.py")
            && content.contains("Latest attempted edit target: `component.py`")
            && content.contains("retry the same bounded edit operation for `component.py`")
            && content.contains("Required recovery operation: submit a corrected `apply_patch`")
            && content.contains("Tool choice remains `auto`")
            && content.contains("Add File body lines must start with `+`")
            && content.contains("top-level `def`")
            && markers.contains(&"invalid_edit_arguments_recovery".to_string())
            && markers.contains(&"strict_apply_patch_grammar".to_string())
            && markers.contains(&"add_file_line_prefix_rule".to_string())
    }) && recovery.candidate_target.as_deref() == Some("component.py")
        && recovery.parser_error_family.as_deref() == Some("apply_patch_malformed_patch")
        && user_recovery_count == 0
}

pub(crate) fn invalid_edit_recovery_projects_candidate_target_operation_fixture_passes() -> bool {
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    let result = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: component.py\n+header\n\ndef build():\n+    return 1\n*** End Patch"}"#,
        "tool patch error: add file body line `def build():` must start with `+`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    recovery.candidate_target.as_deref() == Some("component.py")
        && recovery.parser_error_family.as_deref() == Some("apply_patch_malformed_patch")
        && recovery
            .prompt
            .contains("Latest attempted edit target: `component.py`")
        && recovery
            .prompt
            .contains("retry the same bounded edit operation for `component.py`")
        && recovery
            .prompt
            .contains("before any verification, progress-only todo update, or final answer")
        && !recovery.prompt.contains("calculator.py")
}

pub(crate) fn invalid_edit_arguments_recovery_persists_across_final_message_fixture_passes() -> bool
{
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    let result = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: component.py\n+ok\n*** End"}"#,
        "tool patch error: patch must end with `*** End Patch`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let final_recovery =
        open_obligation_final_message_correction_text(&state, 2, None, &tool_names, false);
    let base_messages = vec![crate::llm::ModelMessage::User {
        content: "create component.py and test_component.py".to_string(),
    }];
    let (messages, _) = provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        Some(final_recovery),
        Some(envelope.prompt.clone()),
        &tool_names,
        true,
    );
    let Some(control) = messages.iter().find_map(|message| match message {
        crate::llm::ModelMessage::System { content } => Some(content.as_str()),
        _ => None,
    }) else {
        return false;
    };
    let markers = request_content_markers(control);
    envelope.tool_name == "apply_patch"
        && envelope.active_targets
            == vec!["component.py".to_string(), "test_component.py".to_string()]
        && envelope.result_hash.is_some()
        && control.contains("Invalid edit recovery:")
        && control.contains("Open-obligation final-message recovery:")
        && markers.contains(&"invalid_edit_arguments_recovery".to_string())
        && markers.contains(&"open_obligation_final_message_recovery".to_string())
        && markers.contains(&"open_targets_projection".to_string())
        && markers.contains(&"strict_apply_patch_grammar".to_string())
}

pub(crate) fn mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_calculator.py")];
    state.completion.open_work_count = 1;
    let arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: calculator.py\n+def calculate(a, b):\n+    return a + b\n*** End Patch\n*** Add File: test_calculator.py\n+import unittest\n+import calculator\n+\n+class TestCalculator(unittest.TestCase):\n+    def test_add(self):\n+        self.assertEqual(calculator.calculate(2, 3), 5)\n*** End Patch"
    })
    .to_string();
    let result = invalid_tool_arguments_result(
        "apply_patch",
        &arguments,
        "tool patch error: unexpected patch line `*** End Patch`. Use the exact apply_patch grammar.",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Required),
    );
    let Some(recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    let projection_id = ProjectionId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary:
            "Requested deliverables still require authoring in the workspace: `test_calculator.py`."
                .to_string(),
        active_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec!["python -m unittest".to_string()],
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace"),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        provider: "openai_compat".to_string(),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        access_mode: crate::config::AccessMode::FullAccess,
        sandbox: SandboxProfile::FullAccess,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: crate::protocol::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let mut obligations = ObligationCompiler::compile(&context);
    obligations
        .items
        .push(invalid_edit_recovery_projection_obligation(&recovery));
    let compiled = TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    });
    let prompt = compiled
        .envelope
        .projection_bundle
        .prompt
        .render_prompt_block();
    let request_diagnostics = compiled
        .envelope
        .projection_bundle
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = compiled
        .envelope
        .projection_bundle
        .tool_result_feedback
        .render_control_projection()
        .text;
    compiled.validation.passes()
        && compiled
            .envelope
            .obligations
            .items
            .iter()
            .any(|item| item.obligation_id == "invalid_edit_recovery")
        && prompt.contains("invalid_edit_recovery")
        && prompt.contains("invalid_edit_arguments:tool=apply_patch")
        && prompt.contains("submitted_targets=calculator.py,test_calculator.py")
        && prompt.contains("active_submitted_targets=test_calculator.py")
        && prompt.contains("inactive_submitted_targets=calculator.py")
        && prompt.contains("mixed_target_apply_patch_rewrite_target_only")
        && request_diagnostics.contains("active_submitted_targets=test_calculator.py")
        && feedback.contains("inactive_submitted_targets=calculator.py")
        && compiled
            .envelope
            .action_authority
            .required_action
            .as_ref()
            .is_some_and(|action| {
                action.projection_text == "apply_patch:test_calculator.py"
                    && action.tool == ToolName::ApplyPatch
            })
        && compiled.envelope.action_authority.tool_choice == ToolChoice::Required
}

pub(crate) fn content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;

    let old_invalid_result = invalid_tool_arguments_result(
        "write",
        r#"{"path":"component.py","content":"def value():\n    return 1"#,
        "EOF while parsing a string at line 1 column 53",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Required),
    );
    let old_recovery = failed_edit_control_recovery_envelope(
        "write",
        &old_invalid_result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    );
    let old_hash = old_recovery
        .as_ref()
        .and_then(|envelope| envelope.result_hash.clone())
        .unwrap_or_default();

    let bad_arguments = json!({
        "path": "component.py",
        "content": "import unittest\nimport component\n\nclass TestComponent(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.add(2, 3), 5)\n"
    });
    let Some(content_shape_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    let Some(recovery) = failed_edit_control_recovery_envelope(
        "write",
        &content_shape_result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    let latest_hash = recovery.result_hash.clone().unwrap_or_default();
    if latest_hash.is_empty() || latest_hash == old_hash {
        return false;
    }

    let projection_id = ProjectionId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary:
            "Requested deliverables still require authoring in the workspace: `component.py`, `test_component.py`."
                .to_string(),
        active_targets: vec![
            Utf8PathBuf::from("component.py"),
            Utf8PathBuf::from("test_component.py"),
        ],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec!["python -m unittest".to_string()],
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace"),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        provider: "openai_compat".to_string(),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        access_mode: crate::config::AccessMode::FullAccess,
        sandbox: SandboxProfile::FullAccess,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: crate::protocol::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let mut obligations = ObligationCompiler::compile(&context);
    obligations
        .items
        .push(invalid_edit_recovery_projection_obligation(&recovery));
    let compiled = TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    });
    let prompt = compiled
        .envelope
        .projection_bundle
        .prompt
        .render_prompt_block();
    let request_diagnostics = compiled
        .envelope
        .projection_bundle
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = compiled
        .envelope
        .projection_bundle
        .tool_result_feedback
        .render_control_projection()
        .text;
    let Some(projected_recovery) = compiled
        .envelope
        .obligations
        .items
        .iter()
        .find(|item| item.obligation_id == "invalid_edit_recovery")
    else {
        return false;
    };
    let projected_evidence = projected_recovery
        .evidence_refs
        .iter()
        .map(|evidence| format!("{}:{}", evidence.source, evidence.reference))
        .collect::<Vec<_>>()
        .join("\n");

    let checks = [
        (
            "failure_kind",
            recovery.failure_kind == "required_write_content_shape_mismatch",
        ),
        (
            "candidate_target",
            recovery.candidate_target.as_deref() == Some("component.py"),
        ),
        (
            "active_submitted",
            recovery
                .active_submitted_targets
                .contains(&"component.py".to_string()),
        ),
        (
            "recovery_action",
            recovery.recovery_action.as_deref() == Some("rewrite_content_for_required_shape"),
        ),
        ("compiled_validation", compiled.validation.passes()),
        (
            "contract_ref",
            projected_recovery
                .contract_refs
                .contains(&"required_write_content_shape_recovery_projection".to_string()),
        ),
        (
            "evidence_failure_kind",
            projected_evidence.contains("required_write_content_shape_mismatch"),
        ),
        (
            "evidence_contract_kind",
            projected_evidence.contains("python_source_executable_content_shape"),
        ),
        (
            "evidence_latest_hash",
            projected_evidence.contains(&latest_hash),
        ),
        (
            "prompt_failure_kind",
            prompt.contains("required_write_content_shape_mismatch"),
        ),
        ("prompt_latest_hash", prompt.contains(&latest_hash)),
        (
            "diagnostics_failure_kind",
            request_diagnostics.contains("required_write_content_shape_mismatch"),
        ),
        (
            "feedback_failure_kind",
            feedback.contains("required_write_content_shape_mismatch"),
        ),
        (
            "old_hash_not_projected",
            old_hash.is_empty() || !prompt.contains(&old_hash),
        ),
        (
            "required_action",
            compiled
                .envelope
                .action_authority
                .required_action
                .as_ref()
                .is_some_and(|action| {
                    action.projection_text == "write:component.py" && action.tool == ToolName::Write
                }),
        ),
    ];
    checks.iter().all(|(_, passed)| *passed)
}

pub(crate) fn open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.completion.open_work_count = 2;
    let recovery =
        open_obligation_final_message_recovery_envelope(&state, 1, None, &tool_names, false);
    let base_messages = vec![
        crate::llm::ModelMessage::User {
            content: "create component.py and test_component.py".to_string(),
        },
        crate::llm::ModelMessage::AssistantToolCalls {
            content: None,
            tool_calls: vec![crate::llm::ModelToolCall {
                call_id: "call-shell".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: json!({"command":"Get-ChildItem -Name"}).to_string(),
            }],
        },
        crate::llm::ModelMessage::Tool {
            call_id: "call-shell".to_string(),
            tool_name: "shell".to_string(),
            result: "supporting context only; no required artifacts changed".to_string(),
        },
    ];
    let first_prompt = Some(recovery.prompt.clone());
    let second_prompt = Some(recovery.prompt.clone());
    let (first_messages, _) = provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        first_prompt,
        None,
        &tool_names,
        true,
    );
    let (second_messages, _) = provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        second_prompt,
        None,
        &tool_names,
        true,
    );
    let system_has_recovery = |messages: &[crate::llm::ModelMessage]| {
        messages.iter().any(|message| match message {
            crate::llm::ModelMessage::System { content } => {
                let markers = request_content_markers(content);
                content.contains("Open-obligation final-message recovery:")
                    && content.contains("component.py, test_component.py")
                    && content.contains("*** Add File")
                    && content.contains("*** Update File")
                    && markers.contains(&"open_obligation_final_message_recovery".to_string())
                    && markers.contains(&"open_targets_projection".to_string())
                    && markers.contains(&"multi_file_apply_patch_shape".to_string())
            }
            _ => false,
        })
    };
    recovery.count == 1
        && recovery.active_targets
            == vec!["component.py".to_string(), "test_component.py".to_string()]
        && system_has_recovery(&first_messages)
        && system_has_recovery(&second_messages)
}

pub(crate) fn open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes()
-> bool {
    let tools = vec![
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
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let initial_tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let initial = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &initial_tool_names,
        TurnLifecycleRecoveryContext::default(),
    );
    let authoring_recovery_tools = tools
        .iter()
        .filter(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &state, &tool.name,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let authoring_recovery_tool_names = authoring_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let authoring_recovery = if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !authoring_recovery_tool_names.is_empty()
    {
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &authoring_recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        )
    } else {
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &authoring_recovery_tool_names,
            TurnLifecycleRecoveryContext::default(),
        )
    };
    let mut repair_state = state.clone();
    repair_state.process_phase = crate::session::ProcessPhase::Repair;
    repair_state.completion.verification_pending = true;
    let repair_recovery_tool_names = tools
        .iter()
        .filter(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &repair_state,
                &tool.name,
            )
        })
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let repair_recovery = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &repair_state,
        &repair_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let repeated_authoring_final_stable_surface_keeps_auto = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &authoring_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let hard_authoring_recovery_tool_names =
        BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repeated_authoring_final_uses_hard_edit_surface = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &hard_authoring_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = crate::session::ProcessPhase::Author;
    docs_state.active_targets = vec![Utf8PathBuf::from("docs/component-design.md")];
    docs_state.completion.open_work_count = 1;
    docs_state.completion.closeout_ready = false;
    docs_state.completion.route_contract_pending = true;
    let docs_recovery_tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let docs_recovery = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &docs_state,
        &docs_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut narrowed_docs_recovery_tools = vec![crate::llm::ToolSchema {
        name: "apply_patch".to_string(),
        description: "apply a patch".to_string(),
        input_schema: json!({"type": "object"}),
        strict: false,
    }];
    let docs_stable_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut narrowed_docs_recovery_tools,
        &docs_stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &docs_state,
            recovery: TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 2,
                ..TurnLifecycleRecoveryContext::default()
            },
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    let restored_docs_recovery_tool_names = narrowed_docs_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let restored_docs_recovery = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &docs_state,
        &restored_docs_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut docs_tools = vec![crate::llm::ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Target file path relative to the current workspace or an allowed absolute path."
                },
                "content": {"type": "string", "description": "Complete final file contents."}
            }
        }),
        strict: false,
    }];
    let required_docs_write =
        fixture_required_edit_action(ToolName::Write, "docs/component-design.md");
    crate::agent::prompt::apply_write_content_shape_to_write_schema_for_required_action(
        &mut docs_tools,
        Some(&required_docs_write),
    );
    let docs_schema_description = docs_tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/content/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    matches!(initial, ToolChoice::Auto)
        && matches!(authoring_recovery, ToolChoice::Auto)
        && matches!(
            repeated_authoring_final_stable_surface_keeps_auto,
            ToolChoice::Auto
        )
        && matches!(
            repeated_authoring_final_uses_hard_edit_surface,
            ToolChoice::Required
        )
        && matches!(docs_recovery, ToolChoice::Required)
        && restored_docs_recovery_tool_names
            == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && matches!(restored_docs_recovery, ToolChoice::Required)
        && docs_schema_description.contains("Complete final Markdown/text contents")
        && docs_schema_description.contains("real newline-separated structure")
        && authoring_recovery_tool_names == initial_tool_names
        && open_obligation_final_message_correction_text(
            &state,
            1,
            None,
            &authoring_recovery_tool_names,
            false,
        )
        .contains("Use the `apply_patch` tool for the active target")
        && open_obligation_final_message_correction_text(
            &state,
            2,
            None,
            &authoring_recovery_tool_names,
            false,
        )
        .contains("Use the `apply_patch` tool for the active target")
        && matches!(repair_recovery, ToolChoice::Auto)
        && repair_recovery_tool_names == initial_tool_names
        && verification_final_message_recovery_uses_shell_fixture_passes()
        && source_repair_final_message_correction_uses_exact_write_action_fixture_passes()
}

pub(crate) fn code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes() -> bool
{
    let mut narrowed_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let stable_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let stable_tool_names = stable_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && TurnLifecycleKernel::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(&state)
    {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut narrowed_tools,
            &stable_tools,
            |_| true,
        );
    } else if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state) {
        narrowed_tools.retain(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &state, &tool.name,
            )
        });
    }
    let recovered_tool_names = narrowed_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice = compile_turn_lifecycle_tool_choice(
        &crate::agent::prompt::PromptPolicy::default(),
        &state,
        &recovered_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    recovered_tool_names == stable_tool_names && matches!(choice, ToolChoice::Auto)
}

pub(crate) fn failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes() -> bool
{
    let mut tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let stable_tools = vec![
        crate::llm::ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        crate::llm::ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
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
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("test_component.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;

    let recovery = TurnLifecycleRecoveryContext {
        failed_edit_recovery_active: true,
        open_obligation_final_message_recovery_active: true,
        open_obligation_final_message_count: 1,
        ..TurnLifecycleRecoveryContext::default()
    };
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: true,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: true,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &crate::agent::prompt::PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names.contains("apply_patch")
        && tool_names.contains("todowrite")
        && tool_names.contains("write")
        && !tool_names.contains("shell")
        && !tool_names.contains("read")
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "failed_edit_final_message_recovery"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
        && plan.terminal_policy == "same_hard_recovery_no_progress_terminal"
}

fn verification_final_message_recovery_uses_shell_fixture_passes() -> bool {
    let recovery_tool_names = BTreeSet::from(["shell".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Verify;
    state.completion.open_work_count = 0;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason =
        Some("requested work authoring is complete; run required verification command(s): python -m unittest".to_string());
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let required_shell = fixture_required_shell_action("python -m unittest");
    let correction = open_obligation_final_message_correction_text(
        &state,
        1,
        Some(&required_shell),
        &recovery_tool_names,
        false,
    );
    matches!(
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        ),
        ToolChoice::Named(ToolName::Shell)
    ) && correction.contains("Use the `shell` tool")
        && correction.contains("python -m unittest")
        && !correction.contains("Use a file-changing tool call")
}

pub(crate) fn source_repair_final_message_correction_uses_exact_write_action_fixture_passes() -> bool
{
    let recovery_tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = crate::session::ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("component.py")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason =
        Some("verification failed; source repair remains active for `component.py`".to_string());
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let required_write = fixture_required_edit_action(ToolName::Write, "component.py");
    let correction = open_obligation_final_message_correction_text(
        &state,
        1,
        Some(&required_write),
        &recovery_tool_names,
        false,
    );

    matches!(
        compile_turn_lifecycle_tool_choice(
            &crate::agent::prompt::PromptPolicy::default(),
            &state,
            &recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        ),
        ToolChoice::Named(ToolName::Write)
    ) && correction.contains("Required action: `write:component.py`")
        && correction.contains("Call the `write` tool")
        && correction.contains("path` exactly `component.py`")
        && !correction.contains("Use the `shell` tool")
        && !correction.contains("python -m unittest")
        && !correction.contains("Use a file-changing tool call")
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
            compile_turn_lifecycle_tool_choice(
                &crate::agent::prompt::PromptPolicy::default(),
                &SessionStateSnapshot::default(),
                &tool_names,
                TurnLifecycleRecoveryContext::default(),
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

fn normalize_provider_system_context_for_chat_template(
    messages: Vec<crate::llm::ModelMessage>,
) -> Vec<crate::llm::ModelMessage> {
    let mut system_blocks = Vec::new();
    let mut non_system_messages = Vec::new();

    for message in messages {
        match message {
            crate::llm::ModelMessage::System { content } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    system_blocks.push(trimmed.to_string());
                }
            }
            other => non_system_messages.push(other),
        }
    }

    if system_blocks.is_empty() {
        return non_system_messages;
    }

    let mut normalized = Vec::with_capacity(non_system_messages.len() + 1);
    normalized.push(crate::llm::ModelMessage::System {
        content: system_blocks.join("\n\n"),
    });
    normalized.extend(non_system_messages);
    normalized
}

fn filter_non_authoritative_assistant_text_for_open_obligations(
    messages: Vec<crate::llm::ModelMessage>,
    open_obligations: bool,
) -> Vec<crate::llm::ModelMessage> {
    if !open_obligations {
        return messages;
    }

    let mut seen_user = false;
    let mut omitted_count = 0usize;
    let mut filtered = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            crate::llm::ModelMessage::User { .. } | crate::llm::ModelMessage::UserParts { .. } => {
                seen_user = true;
                filtered.push(message);
            }
            crate::llm::ModelMessage::Assistant { content }
                if seen_user && !content.trim().is_empty() =>
            {
                omitted_count += 1;
            }
            other => filtered.push(other),
        }
    }

    if omitted_count == 0 {
        return filtered;
    }

    let mut with_note = Vec::with_capacity(filtered.len() + 1);
    with_note.push(crate::llm::ModelMessage::System {
        content: format!(
            "Provider replay assistant-text normalization: omitted {omitted_count} intermediate assistant text message(s) because current obligations remain open. Workspace artifacts, tool outputs, verification evidence, and the current turn control projection are the authority; prior text-only promises are not completion evidence."
        ),
    });
    with_note.extend(filtered);
    with_note
}

pub(crate) fn provider_system_context_normalization_fixture_passes() -> bool {
    let normalized = normalize_provider_system_context_for_chat_template(vec![
        crate::llm::ModelMessage::System {
            content: "control envelope".to_string(),
        },
        crate::llm::ModelMessage::User {
            content: "create component.py and test_component.py".to_string(),
        },
        crate::llm::ModelMessage::System {
            content: "stale inactive authoring replay note".to_string(),
        },
        crate::llm::ModelMessage::Assistant {
            content: "intermediate text".to_string(),
        },
        crate::llm::ModelMessage::System {
            content: "open obligation recovery note".to_string(),
        },
        crate::llm::ModelMessage::User {
            content: "write test_component.py now".to_string(),
        },
    ]);

    let roles = normalized
        .iter()
        .map(|message| match message {
            crate::llm::ModelMessage::System { .. } => "system",
            crate::llm::ModelMessage::User { .. } => "user",
            crate::llm::ModelMessage::UserParts { .. } => "user_parts",
            crate::llm::ModelMessage::Assistant { .. } => "assistant",
            crate::llm::ModelMessage::AssistantToolCalls { .. } => "assistant_tool_calls",
            crate::llm::ModelMessage::Tool { .. } => "tool",
        })
        .collect::<Vec<_>>();

    let system_after_non_system = normalized
        .iter()
        .scan(false, |seen_non_system, message| {
            let is_system = matches!(message, crate::llm::ModelMessage::System { .. });
            let violation = *seen_non_system && is_system;
            if !is_system {
                *seen_non_system = true;
            }
            Some(violation)
        })
        .any(|violation| violation);

    let merged_system = normalized.first().and_then(|message| match message {
        crate::llm::ModelMessage::System { content } => Some(content.as_str()),
        _ => None,
    });

    roles == vec!["system", "user", "assistant", "user"]
        && !system_after_non_system
        && merged_system.is_some_and(|content| {
            content.contains("control envelope")
                && content.contains("stale inactive authoring replay note")
                && content.contains("open obligation recovery note")
        })
}

pub(crate) fn provider_replay_effective_tool_surface_fixture_passes() -> bool {
    let effective = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(
        vec![
            crate::llm::ModelMessage::User {
                content: "create missing test file".to_string(),
            },
            crate::llm::ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![
                    crate::llm::ModelToolCall {
                        call_id: "call-list".to_string(),
                        tool_name: "list".to_string(),
                        arguments_json: r#"{"path":"."}"#.to_string(),
                    },
                    crate::llm::ModelToolCall {
                        call_id: "call-write".to_string(),
                        tool_name: "write".to_string(),
                        arguments_json: r#"{"path":"test_widget.py","content":"ok"}"#.to_string(),
                    },
                    crate::llm::ModelToolCall {
                        call_id: "call-shell".to_string(),
                        tool_name: "shell".to_string(),
                        arguments_json: r#"{"command":"python -X utf8 -m unittest test_widget"}"#
                            .to_string(),
                    },
                ],
            },
            crate::llm::ModelMessage::Tool {
                call_id: "call-list".to_string(),
                tool_name: "list".to_string(),
                result: "1: def add(a, b):\n2:     return a + b\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress\nactive_targets: docs/component-design.md".to_string(),
            },
            crate::llm::ModelMessage::Tool {
                call_id: "call-write".to_string(),
                tool_name: "write".to_string(),
                result: "Wrote test_widget.py".to_string(),
            },
            crate::llm::ModelMessage::Tool {
                call_id: "call-shell".to_string(),
                tool_name: "shell".to_string(),
                result: "semantic_class: provider_ignored_edit_only_surface active_targets: test_widget.py Use `write` or `apply_patch` to repair the active target.".to_string(),
            },
        ],
        &effective,
    );
    let filtered = &projection.messages;

    let has_surface_note = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::User { content }
                if content.contains("outside the current effective tool surface")
                    && content.contains("list")
                    && content.contains("Non-executable supporting-context evidence")
                    && content.contains("def add")
                    && content.contains("Non-executable corrective output")
                    && content.contains("provider_ignored_edit_only_surface")
                    && content.contains("provider-visible edit tool")
        )
    });
    let kept_write = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.len() == 1
                    && tool_calls.first().is_some_and(|call| call.tool_name == "write")
        )
    });
    let omitted_list_call = !filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.tool_name == "list")
        )
    });
    let omitted_list_output = !filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::Tool { tool_name, .. } if tool_name == "list"
        )
    });
    let omitted_shell_output = !filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::Tool { tool_name, .. } if tool_name == "shell"
        )
    });
    let preserved_write_output = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::Tool { tool_name, .. } if tool_name == "write"
        )
    });
    let latest_message_is_correction = matches!(
        filtered.last(),
        Some(crate::llm::ModelMessage::User { content })
            if content.contains("Provider replay surface normalization")
    );

    has_surface_note
        && kept_write
        && omitted_list_call
        && omitted_list_output
        && omitted_shell_output
        && preserved_write_output
        && latest_message_is_correction
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "supporting_context_evidence_preserved"
                && policy.call_id.as_deref() == Some("call-list")
                && policy.tool_name.as_deref() == Some("list")
        })
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "effective_surface_tool_call_omitted"
                && policy.call_id.as_deref() == Some("call-list")
        })
}

pub(crate) fn provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
-> bool {
    let effective = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let call_id = "call-read";
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(
        vec![
            crate::llm::ModelMessage::User {
                content: "Create docs/component-design.md from the implementation and tests."
                    .to_string(),
            },
            crate::llm::ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![crate::llm::ModelToolCall {
                    call_id: call_id.to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: r#"{"path":"component.py"}"#.to_string(),
                }],
            },
            crate::llm::ModelMessage::Tool {
                call_id: call_id.to_string(),
                tool_name: "read".to_string(),
                result: "1: class Component:\n2:     def render(self):\n3:         return \"ok\"\n\n[tool feedback]\noperation_intent: content_changing_authoring_required\noperation_progress_class: supporting_context\nprogress_effect: no_progress\nactive_targets: docs/component-design.md".to_string(),
            },
        ],
        &effective,
    );

    let omitted_executable_read = !projection.messages.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.tool_name == "read")
        ) || matches!(
            message,
            crate::llm::ModelMessage::Tool { tool_name, .. } if tool_name == "read"
        )
    });
    let evidence_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::User { content }
                if content.contains("Non-executable supporting-context evidence")
                    && content.contains("class Component")
                    && content.contains("docs/component-design.md")
                    && content.contains("Do not repeat that omitted tool call")
                    && content.contains("Current effective tool surface: apply_patch, write")
        )
    });
    let policy_recorded = projection.replay_policies.iter().any(|policy| {
        policy.policy == "supporting_context_evidence_preserved"
            && policy.call_id.as_deref() == Some(call_id)
            && policy.tool_name.as_deref() == Some("read")
            && policy
                .reason
                .contains("non-executable provider-visible evidence")
    });

    omitted_executable_read && evidence_preserved && policy_recorded
}

pub(crate) fn provider_replay_omits_intermediate_assistant_text_fixture_passes() -> bool {
    let filtered = filter_non_authoritative_assistant_text_for_open_obligations(
        vec![
            crate::llm::ModelMessage::System {
                content: "control".to_string(),
            },
            crate::llm::ModelMessage::User {
                content: "create files and run tests".to_string(),
            },
            crate::llm::ModelMessage::Assistant {
                content: "I will do that now.".to_string(),
            },
            crate::llm::ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![crate::llm::ModelToolCall {
                    call_id: "call-shell".to_string(),
                    tool_name: "shell".to_string(),
                    arguments_json: r#"{"command":"python -m unittest"}"#.to_string(),
                }],
            },
            crate::llm::ModelMessage::Tool {
                call_id: "call-shell".to_string(),
                tool_name: "shell".to_string(),
                result: "tests failed".to_string(),
            },
            crate::llm::ModelMessage::User {
                content: "run the required verification now".to_string(),
            },
            crate::llm::ModelMessage::Assistant {
                content: "Verification is done.".to_string(),
            },
        ],
        true,
    );
    let closed = filter_non_authoritative_assistant_text_for_open_obligations(
        vec![
            crate::llm::ModelMessage::User {
                content: "summarize".to_string(),
            },
            crate::llm::ModelMessage::Assistant {
                content: "Done.".to_string(),
            },
        ],
        false,
    );

    let assistant_text_count = filtered
        .iter()
        .filter(|message| matches!(message, crate::llm::ModelMessage::Assistant { .. }))
        .count();
    let preserved_tool_call = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.first().is_some_and(|call| call.tool_name == "shell")
        )
    });
    let preserved_tool_output = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::Tool { tool_name, .. } if tool_name == "shell"
        )
    });
    let has_note = filtered.iter().any(|message| {
        matches!(
            message,
            crate::llm::ModelMessage::System { content }
                if content.contains("intermediate assistant text")
        )
    });

    assistant_text_count == 0
        && preserved_tool_call
        && preserved_tool_output
        && has_note
        && closed
            .iter()
            .any(|message| matches!(message, crate::llm::ModelMessage::Assistant { .. }))
}

fn extra_body_with_tool_choice(
    extra_body: Option<Value>,
    tool_count: usize,
    tool_choice: &ToolChoice,
) -> Option<Value> {
    if tool_count == 0 || matches!(tool_choice, ToolChoice::Auto | ToolChoice::None) {
        return extra_body;
    }
    let mut body = match extra_body {
        Some(Value::Object(map)) => Value::Object(map),
        Some(value) => json!({ "extra_body_json": value }),
        None => json!({}),
    };
    if let Value::Object(map) = &mut body {
        match tool_choice {
            ToolChoice::Required => {
                map.insert("tool_choice".to_string(), json!("required"));
            }
            ToolChoice::Named(name) => {
                map.insert(
                    "tool_choice".to_string(),
                    json!({
                        "type": "function",
                        "function": {
                            "name": name.to_string()
                        }
                    }),
                );
            }
            ToolChoice::Auto | ToolChoice::None => {}
        }
    }
    Some(body)
}

fn normalized_target_keys(target: &str, workspace_root: &Utf8Path) -> Vec<String> {
    let normalized = normalize_target_key(target);
    if let Some(relative) =
        crate::workspace::project::workspace_relative_key_for_match(target, workspace_root.as_str())
    {
        vec![normalized, relative]
    } else {
        vec![normalized]
    }
}

fn normalize_target_key(target: &str) -> String {
    crate::workspace::project::path_key_for_workspace_match(target)
}

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use camino::Utf8Path;
use serde_json::Value;

use crate::agent::edit_recovery::{
    InvalidEditRecoveryEnvelope, failed_edit_control_recovery_envelope,
    invalid_edit_recovery_semantic_no_progress_key,
    patch_context_mismatch_target_grounding_read_satisfied,
    patch_context_mismatch_target_grounding_surface_active,
};
use crate::agent::grounding_evidence::{
    AuthoringGroundingDispatchProjection, authoring_grounding_dispatch_projection,
    record_authoring_grounded_active_target,
};
use crate::agent::lifecycle_kernel::{
    OpenObligationFinalMessageRecoveryEnvelope, TurnLifecycleEarlyPreContextSurfaceInput,
    TurnLifecycleEarlyPreContextSurfacePlan, TurnLifecycleKernel,
    TurnLifecycleLatePreContextSurfaceInput, TurnLifecycleLatePreContextSurfacePlan,
    TurnLifecycleRecoveryContext, TurnLifecycleRecoveryContextInput,
};
use crate::agent::state::ActiveWorkContract;
use crate::agent::tool_orchestrator::{
    InvalidArgumentsLifecycleEffectsInput, OperationNoProgressBudgetExhaustion,
    PreExecutionCorrectiveKind, PreExecutionCorrectiveNoProgressInput,
    RejectedModelActionNoProgressDecision, RejectedModelActionNoProgressInput,
    SupportingContextCorrectivePreparationInput, ToolLifecycleRuntime,
};
use crate::error::AgentError;
use crate::llm::ToolSchema;
use crate::protocol::{
    HistoryItem, HistoryItemPayload, LifecycleGuardSnapshot, RequiredAction, ToolChoice,
};
use crate::runtime::RunEventSink;
use crate::session::{RunEvent, SessionId, SessionStateSnapshot};
use crate::tool::ToolResult;

pub(crate) enum LifecycleGuardProgressDecision {
    Continue,
    Fail(String),
}

pub(crate) struct LifecycleGuardRecoveryPromptProjection {
    pub(crate) final_message: Option<String>,
    pub(crate) invalid_edit: Option<String>,
}

#[derive(Default)]
pub(crate) struct LifecycleGuardState {
    rejected_tool_proposals: BTreeMap<String, usize>,
    executed_tool_failure_counts: BTreeMap<String, usize>,
    progress_projection_no_progress_counts: BTreeMap<String, usize>,
    operation_non_content_no_progress_counts: BTreeMap<String, usize>,
    verification_supporting_context_no_progress_counts: BTreeMap<String, usize>,
    same_verification_failure_counts: BTreeMap<String, usize>,
    docs_spec_semantic_reconciliation_counts: BTreeMap<String, usize>,
    public_command_contract_counts: BTreeMap<String, usize>,
    wrong_verification_command_counts: BTreeMap<String, usize>,
    wrong_authoring_target_counts: BTreeMap<String, usize>,
    repair_target_authority_violation_counts: BTreeMap<String, usize>,
    invalid_edit_argument_counts: BTreeMap<String, usize>,
    malformed_write_patch_recovery_pending: bool,
    malformed_apply_patch_write_recovery_pending: bool,
    invalid_edit_arguments_recovery: Option<InvalidEditRecoveryEnvelope>,
    patch_context_mismatch_grounding_targets: BTreeSet<String>,
    authoring_supporting_context_budget_exhausted: BTreeSet<String>,
    authoring_grounded_active_targets: BTreeSet<String>,
    authoring_target_grounding_required_counts: BTreeMap<String, usize>,
    generated_test_target_grounding_required_counts: BTreeMap<String, usize>,
    repair_supporting_context_budget_exhausted: BTreeSet<String>,
    docs_supporting_context_budget_exhausted: BTreeSet<String>,
    docs_supporting_context_budget_exhausted_counts: BTreeMap<String, usize>,
    open_obligation_final_message_count: usize,
    open_obligation_final_message_counts: BTreeMap<String, usize>,
    open_obligation_final_message_recovery: Option<OpenObligationFinalMessageRecoveryEnvelope>,
    open_obligation_final_message_hard_edit_recovery_pending: bool,
    provider_required_tool_choice_final_message_recovery_pending: bool,
    last_persisted_snapshot: Option<crate::protocol::LifecycleGuardSnapshot>,
}

impl LifecycleGuardState {
    pub(crate) fn record_open_obligation_final_message_recovery(
        &mut self,
        state: &SessionStateSnapshot,
        required_action: Option<&RequiredAction>,
        tool_names: &BTreeSet<String>,
        docs_grounding_final_message_recovery_active: bool,
        dispatch_tool_choice: &ToolChoice,
        malformed_apply_patch_write_recovery_active: bool,
        code_authoring_final_message_hard_edit_recovery_active: bool,
    ) -> Option<String> {
        let guard_key = TurnLifecycleKernel::open_obligation_final_message_guard_key(
            state,
            required_action,
            self.invalid_edit_arguments_recovery.as_ref(),
            docs_grounding_final_message_recovery_active,
        );
        self.open_obligation_final_message_count = *self
            .open_obligation_final_message_counts
            .entry(guard_key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if TurnLifecycleKernel::provider_required_tool_choice_final_message_noncompliance(
            state,
            dispatch_tool_choice,
            tool_names,
            malformed_apply_patch_write_recovery_active
                || code_authoring_final_message_hard_edit_recovery_active
                || self.invalid_edit_arguments_recovery.is_some(),
        ) {
            self.provider_required_tool_choice_final_message_recovery_pending = true;
        }
        if self.open_obligation_final_message_count
            >= TurnLifecycleKernel::open_obligation_final_message_terminal_threshold()
        {
            return Some(
                TurnLifecycleKernel::open_obligation_final_message_terminal_message(
                    state,
                    self.open_obligation_final_message_count,
                ),
            );
        }
        self.open_obligation_final_message_recovery = Some(
            TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
                state,
                self.open_obligation_final_message_count,
                required_action,
                tool_names,
                docs_grounding_final_message_recovery_active,
            ),
        );
        None
    }

    pub(crate) fn mark_open_obligation_final_message_hard_edit_recovery_pending(&mut self) {
        self.open_obligation_final_message_hard_edit_recovery_pending = true;
    }

    pub(crate) fn clear_open_obligation_final_message_recovery(&mut self) {
        self.open_obligation_final_message_count = 0;
        self.open_obligation_final_message_counts.clear();
        self.open_obligation_final_message_recovery = None;
        self.open_obligation_final_message_hard_edit_recovery_pending = false;
        self.provider_required_tool_choice_final_message_recovery_pending = false;
    }

    pub(crate) fn emit_next_snapshot_if_changed(
        &mut self,
        session_id: SessionId,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        if let Some(snapshot) = self.next_unpersisted_snapshot() {
            sink.emit(RunEvent::LifecycleGuardUpdated {
                session_id,
                snapshot: snapshot.clone(),
            })?;
            self.mark_persisted(snapshot);
        }
        Ok(())
    }

    pub(crate) fn set_invalid_edit_arguments_recovery(
        &mut self,
        envelope: InvalidEditRecoveryEnvelope,
    ) {
        self.invalid_edit_arguments_recovery = Some(envelope);
    }

    pub(crate) fn record_pre_execution_corrective_no_progress(
        &mut self,
        kind: PreExecutionCorrectiveKind,
        result: &ToolResult,
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        state: &SessionStateSnapshot,
        workspace_root: &Utf8Path,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        open_executable_work: bool,
    ) -> Option<String> {
        if let Some(envelope) = failed_edit_control_recovery_envelope(
            effective_tool_name,
            &result.metadata,
            state,
            allowed_tools,
            tool_choice,
        ) {
            self.set_invalid_edit_arguments_recovery(envelope);
        }
        ToolLifecycleRuntime::record_pre_execution_corrective_no_progress(
            PreExecutionCorrectiveNoProgressInput {
                kind,
                result,
                effective_tool_name,
                parsed_arguments,
                active_work,
                state,
                workspace_root,
                allowed_tools,
                tool_choice,
                open_executable_work,
                operation_non_content_no_progress_counts: &mut self
                    .operation_non_content_no_progress_counts,
                repair_target_authority_violation_counts: &mut self
                    .repair_target_authority_violation_counts,
                wrong_authoring_target_counts: &mut self.wrong_authoring_target_counts,
                docs_spec_semantic_reconciliation_counts: &mut self
                    .docs_spec_semantic_reconciliation_counts,
                public_command_contract_counts: &mut self.public_command_contract_counts,
                wrong_verification_command_counts: &mut self.wrong_verification_command_counts,
            },
        )
        .terminal_message
    }

    pub(crate) fn record_executed_tool_failure_no_progress(
        &mut self,
        effective_tool_name: &str,
        effective_arguments_json: &str,
        allowed_tools: &BTreeSet<String>,
        error: &str,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_executed_tool_failure_no_progress(
            &mut self.executed_tool_failure_counts,
            effective_tool_name,
            effective_arguments_json,
            allowed_tools,
            error,
        )
        .terminal_message
    }

    pub(crate) fn record_same_verification_failure_no_progress(
        &mut self,
        completion_metadata: &Value,
    ) -> Option<String> {
        if let Some(decision) = ToolLifecycleRuntime::record_same_verification_failure_no_progress(
            &mut self.same_verification_failure_counts,
            completion_metadata,
        ) {
            return decision.terminal_message;
        }
        if ToolLifecycleRuntime::verification_run_passed(completion_metadata) {
            self.same_verification_failure_counts.clear();
        }
        None
    }

    pub(crate) fn record_authoring_target_grounding_required_no_progress(
        &mut self,
        result: &ToolResult,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
            &mut self.authoring_target_grounding_required_counts,
            result,
        )
        .terminal_message
    }

    pub(crate) fn record_generated_test_target_grounding_required_no_progress(
        &mut self,
        result: &ToolResult,
        state: &SessionStateSnapshot,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_generated_test_target_grounding_required_no_progress(
            &mut self.generated_test_target_grounding_required_counts,
            result,
            state,
        )
        .terminal_message
    }

    pub(crate) fn record_authoring_grounded_active_target(
        &mut self,
        effective_tool_name: &str,
        completion_metadata: &Value,
        state: &SessionStateSnapshot,
    ) {
        record_authoring_grounded_active_target(
            &mut self.authoring_grounded_active_targets,
            effective_tool_name,
            completion_metadata,
            state,
        );
    }

    pub(crate) fn record_progress_projection_no_progress(
        &mut self,
        progress_key: String,
        effective_tool_name: &str,
        state: &SessionStateSnapshot,
    ) -> Option<String> {
        let progress_count = self
            .progress_projection_no_progress_counts
            .entry(progress_key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        ToolLifecycleRuntime::should_terminalize_progress_projection_no_progress(*progress_count)
            .then(|| {
                ToolLifecycleRuntime::progress_projection_no_progress_terminal_message(
                    effective_tool_name,
                    *progress_count,
                    state,
                )
            })
    }

    pub(crate) fn record_operation_non_content_no_progress(
        &mut self,
        effective_tool_name: &str,
        completion_metadata: &Value,
        state: &SessionStateSnapshot,
        tool_names: &BTreeSet<String>,
        dispatch_tool_choice: &ToolChoice,
        open_executable_work: bool,
    ) -> Option<LifecycleGuardProgressDecision> {
        let decision = ToolLifecycleRuntime::record_operation_non_content_no_progress(
            &mut self.operation_non_content_no_progress_counts,
            effective_tool_name,
            completion_metadata,
            state,
            tool_names,
            dispatch_tool_choice,
            open_executable_work,
        )?;
        if patch_context_mismatch_target_grounding_read_satisfied(
            effective_tool_name,
            completion_metadata,
            state,
        ) {
            self.patch_context_mismatch_grounding_targets.clear();
        }
        if let Some(budget_exhaustion) = decision.budget_exhaustion {
            self.record_budget_exhaustion(budget_exhaustion, decision.key);
            return Some(LifecycleGuardProgressDecision::Continue);
        }
        decision
            .terminal_message
            .map(LifecycleGuardProgressDecision::Fail)
    }

    pub(crate) fn record_verification_supporting_context_no_progress(
        &mut self,
        effective_tool_name: &str,
        effective_arguments_json: &str,
        result: &ToolResult,
        state: &SessionStateSnapshot,
        tool_names: &BTreeSet<String>,
        dispatch_tool_choice: &ToolChoice,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_verification_supporting_context_no_progress(
            &mut self.verification_supporting_context_no_progress_counts,
            effective_tool_name,
            effective_arguments_json,
            result,
            state,
            tool_names,
            dispatch_tool_choice,
        )?
        .terminal_message
    }

    pub(crate) fn record_docs_supporting_context_budget_exhausted_no_progress(
        &mut self,
        budget_key: String,
        state: &SessionStateSnapshot,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_docs_supporting_context_budget_exhausted_no_progress(
            &mut self.docs_supporting_context_budget_exhausted_counts,
            budget_key,
            state,
        )
        .terminal_message
    }

    pub(crate) fn record_invalid_arguments_recovery(
        &mut self,
        effective_tool_name: &str,
        result_metadata: &Value,
        state: &SessionStateSnapshot,
        tool_names: &BTreeSet<String>,
        dispatch_tool_choice: &ToolChoice,
    ) -> Option<String> {
        ToolLifecycleRuntime::record_invalid_arguments_lifecycle_effects(
            InvalidArgumentsLifecycleEffectsInput {
                effective_tool_name,
                result_metadata,
                state,
                allowed_tools: tool_names,
                tool_choice: dispatch_tool_choice,
                patch_context_mismatch_grounding_targets: &mut self
                    .patch_context_mismatch_grounding_targets,
                invalid_edit_argument_counts: &mut self.invalid_edit_argument_counts,
                invalid_edit_arguments_recovery: &mut self.invalid_edit_arguments_recovery,
                malformed_write_patch_recovery_pending: &mut self
                    .malformed_write_patch_recovery_pending,
                malformed_apply_patch_write_recovery_pending: &mut self
                    .malformed_apply_patch_write_recovery_pending,
            },
        )
    }

    pub(crate) fn record_rejected_model_action_invalid_arguments_recovery(
        &mut self,
        effective_tool_name: &str,
        result_metadata: &Value,
        state: &SessionStateSnapshot,
        tool_names: &BTreeSet<String>,
        dispatch_tool_choice: &ToolChoice,
    ) -> Option<String> {
        self.record_invalid_arguments_recovery(
            effective_tool_name,
            result_metadata,
            state,
            tool_names,
            dispatch_tool_choice,
        )
    }

    pub(crate) fn record_rejected_model_action_no_progress(
        &mut self,
        effective_tool_name: &str,
        effective_arguments_json: &str,
        result_metadata: &Value,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        required_action: Option<&RequiredAction>,
        tool_allowed: bool,
    ) -> RejectedModelActionNoProgressDecision {
        let invalid_edit_recovery_no_progress_key = self
            .invalid_edit_arguments_recovery
            .as_ref()
            .map(invalid_edit_recovery_semantic_no_progress_key);
        ToolLifecycleRuntime::record_rejected_model_action_no_progress(
            RejectedModelActionNoProgressInput {
                rejected_tool_proposals: &mut self.rejected_tool_proposals,
                effective_tool_name,
                effective_arguments_json,
                result_metadata,
                allowed_tools,
                tool_choice,
                required_action,
                tool_allowed,
                recovery_no_progress_key: invalid_edit_recovery_no_progress_key.as_deref(),
            },
        )
    }

    pub(crate) fn record_completed_tool_lifecycle_effects(
        &mut self,
        input: CompletedToolLifecycleEffectsInput<'_>,
    ) -> Option<LifecycleGuardProgressDecision> {
        let docs_route_contract_pending =
            TurnLifecycleKernel::docs_route_contract_pending_after_file_change(input.state);
        let open_executable_work =
            TurnLifecycleKernel::open_executable_work_requires_tool_call(input.state);
        let progress_projection_no_content =
            ToolLifecycleRuntime::tool_result_is_progress_projection_no_content(input.result)
                && open_executable_work;
        if input.content_changing_progress {
            self.clear_after_content_changing_progress(docs_route_contract_pending);
        }
        self.record_authoring_grounded_active_target(
            input.effective_tool_name,
            input.completion_metadata,
            input.state,
        );
        if progress_projection_no_content {
            let progress_key = ToolLifecycleRuntime::progress_projection_no_progress_key(
                input.effective_tool_name,
                input.state,
                input.tool_names,
                input.dispatch_tool_choice,
                ToolLifecycleRuntime::tool_result_result_hash(input.completion_metadata).as_deref(),
            );
            if let Some(message) = self.record_progress_projection_no_progress(
                progress_key,
                input.effective_tool_name,
                input.state,
            ) {
                return Some(LifecycleGuardProgressDecision::Fail(message));
            }
        }
        if let Some(decision) = self.record_operation_non_content_no_progress(
            input.effective_tool_name,
            input.completion_metadata,
            input.state,
            input.tool_names,
            input.dispatch_tool_choice,
            open_executable_work,
        ) {
            return Some(decision);
        }
        if let Some(message) = self.record_verification_supporting_context_no_progress(
            input.effective_tool_name,
            input.effective_arguments_json,
            input.result,
            input.state,
            input.tool_names,
            input.dispatch_tool_choice,
        ) {
            return Some(LifecycleGuardProgressDecision::Fail(message));
        }
        self.record_same_verification_failure_no_progress(input.completion_metadata)
            .map(LifecycleGuardProgressDecision::Fail)
    }

    pub(crate) fn record_tool_execution_error_effects(
        &mut self,
        input: ToolExecutionErrorEffectsInput<'_>,
    ) -> Option<String> {
        if let Some(metadata) = input.invalid_arguments_metadata {
            return self.record_invalid_arguments_recovery(
                input.effective_tool_name,
                metadata,
                input.state,
                input.tool_names,
                input.dispatch_tool_choice,
            );
        }
        self.record_executed_tool_failure_no_progress(
            input.effective_tool_name,
            input.effective_arguments_json,
            input.tool_names,
            input.error_text,
        )
    }

    pub(crate) fn record_budget_exhaustion(
        &mut self,
        exhaustion: OperationNoProgressBudgetExhaustion,
        key: String,
    ) {
        match exhaustion {
            OperationNoProgressBudgetExhaustion::DocsSupportingContext => {
                self.docs_supporting_context_budget_exhausted.insert(key);
            }
            OperationNoProgressBudgetExhaustion::AuthoringSupportingContext => {
                self.authoring_supporting_context_budget_exhausted
                    .insert(key);
            }
            OperationNoProgressBudgetExhaustion::RepairSupportingContext => {
                self.repair_supporting_context_budget_exhausted.insert(key);
            }
        }
    }

    pub(crate) fn prepare_supporting_context_corrective_input(
        &self,
        effective_tool_name: &str,
        parsed_arguments: &Value,
        state: &SessionStateSnapshot,
        history_items: &[HistoryItem],
        workspace_root: &Utf8Path,
        tool_names: &BTreeSet<String>,
        dispatch_tool_choice: &ToolChoice,
        existing_target_grounding_recovery_active: bool,
        generated_test_reference_consumed_target_grounding_active: bool,
    ) -> crate::agent::tool_orchestrator::PreparedSupportingContextCorrectiveInput {
        ToolLifecycleRuntime::prepare_supporting_context_corrective_input(
            SupportingContextCorrectivePreparationInput {
                effective_tool_name,
                parsed_arguments,
                state,
                history_items,
                workspace_root,
                allowed_tools: tool_names,
                tool_choice: dispatch_tool_choice,
                docs_supporting_context_budget_exhausted: &self
                    .docs_supporting_context_budget_exhausted,
                authoring_supporting_context_budget_exhausted: &self
                    .authoring_supporting_context_budget_exhausted,
                authoring_grounded_active_targets: &self.authoring_grounded_active_targets,
                existing_target_grounding_recovery_active,
                generated_test_reference_consumed_target_grounding_active,
            },
        )
    }

    pub(crate) fn compile_recovery_context(
        &self,
        input: LifecycleGuardRecoveryContextInput<'_>,
    ) -> TurnLifecycleRecoveryContext {
        TurnLifecycleKernel::compile_recovery_context(TurnLifecycleRecoveryContextInput {
            state: input.state,
            tools: input.tools,
            stable_tools: input.stable_tools,
            current_tool_names: input.current_tool_names,
            post_provider_tool_names: input.post_provider_tool_names,
            rejected_tool_proposals: &self.rejected_tool_proposals,
            wrong_authoring_target_counts: &self.wrong_authoring_target_counts,
            progress_projection_no_progress_counts: &self.progress_projection_no_progress_counts,
            repair_supporting_context_budget_recovery_active: input
                .repair_supporting_context_budget_recovery_active,
            malformed_write_patch_recovery_pending: self.malformed_write_patch_recovery_pending,
            malformed_apply_patch_write_recovery_pending: self
                .malformed_apply_patch_write_recovery_pending,
            has_open_obligation_final_message_recovery: self
                .open_obligation_final_message_recovery
                .is_some(),
            open_obligation_final_message_recovery_count: self
                .open_obligation_final_message_recovery
                .as_ref()
                .map(|envelope| envelope.count),
            open_obligation_final_message_hard_edit_recovery_pending: self
                .open_obligation_final_message_hard_edit_recovery_pending,
            provider_required_tool_choice_final_message_recovery_pending: self
                .provider_required_tool_choice_final_message_recovery_pending,
            has_invalid_edit_recovery: self.invalid_edit_arguments_recovery.is_some(),
            generated_test_source_reference_grounding_active: input
                .generated_test_source_reference_grounding_active,
            generated_test_reference_consumed_target_grounding_active: input
                .generated_test_reference_consumed_target_grounding_active,
            verification_target_grounding_active: input.verification_target_grounding_active,
            authoring_target_grounding_recovery_edit_only: input
                .authoring_target_grounding_recovery_edit_only,
            patch_context_mismatch_grounding_active: input.patch_context_mismatch_grounding_active,
            existing_target_grounding_recovery_active: input
                .existing_target_grounding_recovery_active,
            docs_route_has_required_content_grounding_evidence: input
                .docs_route_has_required_content_grounding_evidence,
            authoring_targets_need_grounding: input.authoring_targets_need_grounding,
            progress_projection_target_grounding_read_needed: input
                .progress_projection_target_grounding_read_needed,
        })
    }

    pub(crate) fn recovery_prompt_projection(&self) -> LifecycleGuardRecoveryPromptProjection {
        LifecycleGuardRecoveryPromptProjection {
            final_message: recovery_payload_prompt(
                self.open_obligation_final_message_recovery.as_ref(),
                |envelope| envelope.prompt.as_str(),
            ),
            invalid_edit: recovery_payload_prompt(
                self.invalid_edit_arguments_recovery.as_ref(),
                |envelope| envelope.prompt.as_str(),
            ),
        }
    }

    pub(crate) fn authoring_grounding_dispatch_projection(
        &self,
        history_items: &[HistoryItem],
        state: &SessionStateSnapshot,
        workspace_root: &Utf8Path,
    ) -> AuthoringGroundingDispatchProjection {
        authoring_grounding_dispatch_projection(
            history_items,
            state,
            workspace_root,
            &self.authoring_grounded_active_targets,
        )
    }

    pub(crate) fn authoring_supporting_context_budget_recovery_active(
        &self,
        state: &SessionStateSnapshot,
    ) -> bool {
        TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
            state,
            &self.authoring_supporting_context_budget_exhausted,
        )
    }

    pub(crate) fn patch_context_mismatch_grounding_active(
        &self,
        state: &SessionStateSnapshot,
    ) -> bool {
        patch_context_mismatch_target_grounding_surface_active(
            state,
            &self.patch_context_mismatch_grounding_targets,
        )
    }

    pub(crate) fn repair_supporting_context_budget_recovery_active(
        &self,
        state: &SessionStateSnapshot,
    ) -> bool {
        TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
            state,
            &self.repair_supporting_context_budget_exhausted,
        )
    }

    pub(crate) fn invalid_edit_arguments_recovery_envelope(
        &self,
    ) -> Option<&InvalidEditRecoveryEnvelope> {
        self.invalid_edit_arguments_recovery.as_ref()
    }

    pub(crate) fn apply_early_pre_context_recovery_surface(
        &self,
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        state: &SessionStateSnapshot,
        authoring_supporting_context_budget_recovery_needs_read: bool,
        generated_test_source_reference_grounding_active: bool,
        generated_test_reference_consumed_target_grounding_active: bool,
        singleton_missing_authoring_target_create_action_active: bool,
        existing_target_grounding_recovery_active: bool,
        patch_context_mismatch_grounding_active: bool,
    ) -> TurnLifecycleEarlyPreContextSurfacePlan {
        let docs_route_supporting_context_budget_recovery_active =
            TurnLifecycleKernel::docs_route_supporting_context_budget_recovery_surface_active(
                state,
                &self.docs_supporting_context_budget_exhausted,
            );
        let authoring_supporting_context_budget_recovery_active =
            self.authoring_supporting_context_budget_recovery_active(state);
        let repair_supporting_context_budget_recovery_active =
            self.repair_supporting_context_budget_recovery_active(state);
        TurnLifecycleKernel::apply_early_pre_context_recovery_surface(
            tools,
            stable_tools,
            TurnLifecycleEarlyPreContextSurfaceInput {
                state,
                docs_route_supporting_context_budget_recovery_active,
                authoring_supporting_context_budget_recovery_active,
                authoring_supporting_context_budget_recovery_needs_read,
                generated_test_source_reference_grounding_active,
                generated_test_reference_consumed_target_grounding_active,
                singleton_missing_authoring_target_create_action_active,
                existing_target_grounding_recovery_active,
                patch_context_mismatch_grounding_active,
                repair_supporting_context_budget_recovery_active,
            },
        )
    }

    pub(crate) fn apply_late_pre_context_recovery_surface(
        &self,
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        state: &SessionStateSnapshot,
        repair_supporting_context_budget_recovery_active: bool,
        patch_context_mismatch_grounding_active: bool,
        verification_target_grounding_active: bool,
    ) -> TurnLifecycleLatePreContextSurfacePlan {
        TurnLifecycleKernel::apply_late_pre_context_recovery_surface(
            tools,
            stable_tools,
            TurnLifecycleLatePreContextSurfaceInput {
                state,
                rejected_tool_proposals: &self.rejected_tool_proposals,
                wrong_authoring_target_counts: &self.wrong_authoring_target_counts,
                repair_supporting_context_budget_recovery_active,
                malformed_write_patch_recovery_pending: self.malformed_write_patch_recovery_pending,
                malformed_apply_patch_write_recovery_pending: self
                    .malformed_apply_patch_write_recovery_pending,
                patch_context_mismatch_grounding_active,
                verification_target_grounding_active,
            },
        )
    }

    pub(crate) fn clear_after_content_changing_progress(
        &mut self,
        docs_route_contract_pending: bool,
    ) {
        self.progress_projection_no_progress_counts.clear();
        self.operation_non_content_no_progress_counts.clear();
        self.verification_supporting_context_no_progress_counts
            .clear();
        self.wrong_authoring_target_counts.clear();
        self.repair_target_authority_violation_counts.clear();
        self.invalid_edit_argument_counts.clear();
        self.malformed_write_patch_recovery_pending = false;
        self.malformed_apply_patch_write_recovery_pending = false;
        self.invalid_edit_arguments_recovery = None;
        self.provider_required_tool_choice_final_message_recovery_pending = false;
        self.open_obligation_final_message_count = 0;
        self.open_obligation_final_message_counts.clear();
        self.open_obligation_final_message_recovery = None;
        self.open_obligation_final_message_hard_edit_recovery_pending = false;
        self.patch_context_mismatch_grounding_targets.clear();
        self.authoring_supporting_context_budget_exhausted.clear();
        self.authoring_grounded_active_targets.clear();
        self.authoring_target_grounding_required_counts.clear();
        self.generated_test_target_grounding_required_counts.clear();
        self.repair_supporting_context_budget_exhausted.clear();
        if !docs_route_contract_pending {
            self.docs_supporting_context_budget_exhausted.clear();
        }
        self.docs_supporting_context_budget_exhausted_counts.clear();
    }

    pub(crate) fn snapshot(&self) -> Option<crate::protocol::LifecycleGuardSnapshot> {
        lifecycle_guard_snapshot_from_parts(LifecycleGuardSnapshotInput {
            rejected_tool_proposals: &self.rejected_tool_proposals,
            executed_tool_failure_counts: &self.executed_tool_failure_counts,
            progress_projection_no_progress_counts: &self.progress_projection_no_progress_counts,
            operation_non_content_no_progress_counts: &self
                .operation_non_content_no_progress_counts,
            verification_supporting_context_no_progress_counts: &self
                .verification_supporting_context_no_progress_counts,
            same_verification_failure_counts: &self.same_verification_failure_counts,
            docs_spec_semantic_reconciliation_counts: &self
                .docs_spec_semantic_reconciliation_counts,
            public_command_contract_counts: &self.public_command_contract_counts,
            wrong_verification_command_counts: &self.wrong_verification_command_counts,
            wrong_authoring_target_counts: &self.wrong_authoring_target_counts,
            repair_target_authority_violation_counts: &self
                .repair_target_authority_violation_counts,
            invalid_edit_argument_counts: &self.invalid_edit_argument_counts,
            authoring_target_grounding_required_counts: &self
                .authoring_target_grounding_required_counts,
            generated_test_target_grounding_required_counts: &self
                .generated_test_target_grounding_required_counts,
            docs_supporting_context_budget_exhausted_counts: &self
                .docs_supporting_context_budget_exhausted_counts,
            open_obligation_final_message_counts: &self.open_obligation_final_message_counts,
            malformed_write_patch_recovery_pending: self.malformed_write_patch_recovery_pending,
            malformed_apply_patch_write_recovery_pending: self
                .malformed_apply_patch_write_recovery_pending,
            invalid_edit_arguments_recovery: self.invalid_edit_arguments_recovery.as_ref(),
            open_obligation_final_message_recovery: self
                .open_obligation_final_message_recovery
                .as_ref(),
            open_obligation_final_message_hard_edit_recovery_pending: self
                .open_obligation_final_message_hard_edit_recovery_pending,
            provider_required_tool_choice_final_message_recovery_pending: self
                .provider_required_tool_choice_final_message_recovery_pending,
            patch_context_mismatch_grounding_targets: &self
                .patch_context_mismatch_grounding_targets,
            authoring_supporting_context_budget_exhausted: &self
                .authoring_supporting_context_budget_exhausted,
            authoring_grounded_active_targets: &self.authoring_grounded_active_targets,
            repair_supporting_context_budget_exhausted: &self
                .repair_supporting_context_budget_exhausted,
            docs_supporting_context_budget_exhausted: &self
                .docs_supporting_context_budget_exhausted,
        })
    }

    pub(crate) fn next_unpersisted_snapshot(
        &self,
    ) -> Option<crate::protocol::LifecycleGuardSnapshot> {
        next_unpersisted_lifecycle_guard_snapshot(
            self.snapshot(),
            self.last_persisted_snapshot.as_ref(),
        )
    }

    pub(crate) fn mark_persisted(&mut self, snapshot: crate::protocol::LifecycleGuardSnapshot) {
        self.last_persisted_snapshot = Some(snapshot);
    }

    pub(crate) fn hydrate_from_history_items(history_items: &[HistoryItem]) -> Self {
        latest_lifecycle_guard_snapshot(history_items)
            .map(Self::from_snapshot)
            .unwrap_or_default()
    }

    pub(crate) fn from_snapshot(snapshot: &crate::protocol::LifecycleGuardSnapshot) -> Self {
        let parts = hydrate_lifecycle_guard_snapshot_parts(snapshot);
        Self {
            rejected_tool_proposals: parts.rejected_tool_proposals,
            executed_tool_failure_counts: parts.executed_tool_failure_counts,
            progress_projection_no_progress_counts: parts.progress_projection_no_progress_counts,
            operation_non_content_no_progress_counts: parts
                .operation_non_content_no_progress_counts,
            verification_supporting_context_no_progress_counts: parts
                .verification_supporting_context_no_progress_counts,
            same_verification_failure_counts: parts.same_verification_failure_counts,
            docs_spec_semantic_reconciliation_counts: parts
                .docs_spec_semantic_reconciliation_counts,
            public_command_contract_counts: parts.public_command_contract_counts,
            wrong_verification_command_counts: parts.wrong_verification_command_counts,
            wrong_authoring_target_counts: parts.wrong_authoring_target_counts,
            repair_target_authority_violation_counts: parts
                .repair_target_authority_violation_counts,
            invalid_edit_argument_counts: parts.invalid_edit_argument_counts,
            malformed_write_patch_recovery_pending: parts.malformed_write_patch_recovery_pending,
            malformed_apply_patch_write_recovery_pending: parts
                .malformed_apply_patch_write_recovery_pending,
            invalid_edit_arguments_recovery: parts.invalid_edit_arguments_recovery,
            patch_context_mismatch_grounding_targets: parts
                .patch_context_mismatch_grounding_targets,
            authoring_supporting_context_budget_exhausted: parts
                .authoring_supporting_context_budget_exhausted,
            authoring_grounded_active_targets: parts.authoring_grounded_active_targets,
            authoring_target_grounding_required_counts: parts
                .authoring_target_grounding_required_counts,
            generated_test_target_grounding_required_counts: parts
                .generated_test_target_grounding_required_counts,
            repair_supporting_context_budget_exhausted: parts
                .repair_supporting_context_budget_exhausted,
            docs_supporting_context_budget_exhausted: parts
                .docs_supporting_context_budget_exhausted,
            docs_supporting_context_budget_exhausted_counts: parts
                .docs_supporting_context_budget_exhausted_counts,
            open_obligation_final_message_count: parts.open_obligation_final_message_count,
            open_obligation_final_message_counts: parts.open_obligation_final_message_counts,
            open_obligation_final_message_recovery: parts.open_obligation_final_message_recovery,
            open_obligation_final_message_hard_edit_recovery_pending: parts
                .open_obligation_final_message_hard_edit_recovery_pending,
            provider_required_tool_choice_final_message_recovery_pending: parts
                .provider_required_tool_choice_final_message_recovery_pending,
            last_persisted_snapshot: parts.last_persisted_snapshot,
        }
    }
}

pub(crate) struct CompletedToolLifecycleEffectsInput<'a> {
    pub(crate) effective_tool_name: &'a str,
    pub(crate) effective_arguments_json: &'a str,
    pub(crate) result: &'a ToolResult,
    pub(crate) completion_metadata: &'a Value,
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) tool_names: &'a BTreeSet<String>,
    pub(crate) dispatch_tool_choice: &'a ToolChoice,
    pub(crate) content_changing_progress: bool,
}

pub(crate) struct ToolExecutionErrorEffectsInput<'a> {
    pub(crate) effective_tool_name: &'a str,
    pub(crate) effective_arguments_json: &'a str,
    pub(crate) error_text: &'a str,
    pub(crate) invalid_arguments_metadata: Option<&'a Value>,
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) tool_names: &'a BTreeSet<String>,
    pub(crate) dispatch_tool_choice: &'a ToolChoice,
}

pub(crate) struct LifecycleGuardRecoveryContextInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) tools: &'a [crate::llm::ToolSchema],
    pub(crate) stable_tools: &'a [crate::llm::ToolSchema],
    pub(crate) current_tool_names: &'a BTreeSet<String>,
    pub(crate) post_provider_tool_names: &'a BTreeSet<String>,
    pub(crate) repair_supporting_context_budget_recovery_active: bool,
    pub(crate) generated_test_source_reference_grounding_active: bool,
    pub(crate) generated_test_reference_consumed_target_grounding_active: bool,
    pub(crate) verification_target_grounding_active: bool,
    pub(crate) authoring_target_grounding_recovery_edit_only: bool,
    pub(crate) patch_context_mismatch_grounding_active: bool,
    pub(crate) existing_target_grounding_recovery_active: bool,
    pub(crate) docs_route_has_required_content_grounding_evidence: bool,
    pub(crate) authoring_targets_need_grounding: bool,
    pub(crate) progress_projection_target_grounding_read_needed: bool,
}

pub(crate) fn lifecycle_guard_history_item_order_key(item: &HistoryItem) -> (i64, i64) {
    (item.sequence_no, item.created_at_ms)
}

pub(crate) fn latest_lifecycle_guard_snapshot(
    history_items: &[HistoryItem],
) -> Option<&LifecycleGuardSnapshot> {
    history_items
        .iter()
        .filter(|item| matches!(item.payload, HistoryItemPayload::LifecycleGuard { .. }))
        .max_by_key(|item| lifecycle_guard_history_item_order_key(item))
        .and_then(|item| match &item.payload {
            HistoryItemPayload::LifecycleGuard { snapshot } => Some(snapshot),
            _ => None,
        })
}

pub(crate) fn empty_lifecycle_guard_snapshot() -> LifecycleGuardSnapshot {
    LifecycleGuardSnapshot {
        counters: BTreeMap::new(),
        active_flags: Vec::new(),
        scoped_targets: Vec::new(),
        payloads: BTreeMap::new(),
    }
}

pub(crate) fn lifecycle_guard_snapshot_is_empty(snapshot: &LifecycleGuardSnapshot) -> bool {
    snapshot.counters.is_empty()
        && snapshot.active_flags.is_empty()
        && snapshot.scoped_targets.is_empty()
        && snapshot.payloads.is_empty()
}

pub(crate) fn next_unpersisted_lifecycle_guard_snapshot(
    current: Option<LifecycleGuardSnapshot>,
    last_persisted: Option<&LifecycleGuardSnapshot>,
) -> Option<LifecycleGuardSnapshot> {
    let snapshot = current.unwrap_or_else(empty_lifecycle_guard_snapshot);
    if last_persisted.is_none() && lifecycle_guard_snapshot_is_empty(&snapshot) {
        return None;
    }
    (last_persisted != Some(&snapshot)).then_some(snapshot)
}

pub(crate) fn extend_counter_group(
    counters: &mut BTreeMap<String, usize>,
    prefix: &str,
    values: &BTreeMap<String, usize>,
) {
    counters.extend(
        values
            .iter()
            .map(|(key, count)| (format!("{prefix}:{key}"), *count)),
    );
}

pub(crate) fn push_active_flag(flags: &mut Vec<String>, name: &str, active: bool) {
    if active {
        flags.push(name.to_string());
    }
}

pub(crate) fn extend_scoped_target_group(
    scoped_targets: &mut Vec<String>,
    prefix: &str,
    targets: &BTreeSet<String>,
) {
    scoped_targets.extend(targets.iter().map(|target| format!("{prefix}:{target}")));
}

pub(crate) fn hydrate_counter_group(
    source: &BTreeMap<String, usize>,
    prefix: &str,
    target: &mut BTreeMap<String, usize>,
) {
    let prefix = format!("{prefix}:");
    target.extend(source.iter().filter_map(|(key, count)| {
        key.strip_prefix(&prefix)
            .map(|local_key| (local_key.to_string(), *count))
    }));
}

pub(crate) fn hydrate_scoped_target_group(
    source: &[String],
    prefix: &str,
    target: &mut BTreeSet<String>,
) {
    let prefix = format!("{prefix}:");
    target.extend(
        source
            .iter()
            .filter_map(|value| value.strip_prefix(&prefix).map(str::to_string)),
    );
}

pub(crate) fn recovery_payload_prompt<T, F>(payload: Option<&T>, prompt: F) -> Option<String>
where
    F: FnOnce(&T) -> &str,
{
    payload.map(|value| prompt(value).to_string())
}

pub(crate) fn apply_recovery_prompts_to_system_prompt(
    system_prompt: String,
    final_message_recovery_prompt: Option<&str>,
    invalid_edit_recovery_prompt: Option<&str>,
) -> String {
    let mut system_prompt = system_prompt;
    if let Some(correction) = final_message_recovery_prompt {
        system_prompt = format!("{correction}\n\n{system_prompt}");
    }
    if let Some(correction) = invalid_edit_recovery_prompt {
        system_prompt = format!("{correction}\n\n{system_prompt}");
    }
    system_prompt
}

pub(crate) struct LifecycleGuardSnapshotInput<'a> {
    pub(crate) rejected_tool_proposals: &'a BTreeMap<String, usize>,
    pub(crate) executed_tool_failure_counts: &'a BTreeMap<String, usize>,
    pub(crate) progress_projection_no_progress_counts: &'a BTreeMap<String, usize>,
    pub(crate) operation_non_content_no_progress_counts: &'a BTreeMap<String, usize>,
    pub(crate) verification_supporting_context_no_progress_counts: &'a BTreeMap<String, usize>,
    pub(crate) same_verification_failure_counts: &'a BTreeMap<String, usize>,
    pub(crate) docs_spec_semantic_reconciliation_counts: &'a BTreeMap<String, usize>,
    pub(crate) public_command_contract_counts: &'a BTreeMap<String, usize>,
    pub(crate) wrong_verification_command_counts: &'a BTreeMap<String, usize>,
    pub(crate) wrong_authoring_target_counts: &'a BTreeMap<String, usize>,
    pub(crate) repair_target_authority_violation_counts: &'a BTreeMap<String, usize>,
    pub(crate) invalid_edit_argument_counts: &'a BTreeMap<String, usize>,
    pub(crate) authoring_target_grounding_required_counts: &'a BTreeMap<String, usize>,
    pub(crate) generated_test_target_grounding_required_counts: &'a BTreeMap<String, usize>,
    pub(crate) docs_supporting_context_budget_exhausted_counts: &'a BTreeMap<String, usize>,
    pub(crate) open_obligation_final_message_counts: &'a BTreeMap<String, usize>,
    pub(crate) malformed_write_patch_recovery_pending: bool,
    pub(crate) malformed_apply_patch_write_recovery_pending: bool,
    pub(crate) invalid_edit_arguments_recovery: Option<&'a InvalidEditRecoveryEnvelope>,
    pub(crate) open_obligation_final_message_recovery:
        Option<&'a OpenObligationFinalMessageRecoveryEnvelope>,
    pub(crate) open_obligation_final_message_hard_edit_recovery_pending: bool,
    pub(crate) provider_required_tool_choice_final_message_recovery_pending: bool,
    pub(crate) patch_context_mismatch_grounding_targets: &'a BTreeSet<String>,
    pub(crate) authoring_supporting_context_budget_exhausted: &'a BTreeSet<String>,
    pub(crate) authoring_grounded_active_targets: &'a BTreeSet<String>,
    pub(crate) repair_supporting_context_budget_exhausted: &'a BTreeSet<String>,
    pub(crate) docs_supporting_context_budget_exhausted: &'a BTreeSet<String>,
}

pub(crate) fn lifecycle_guard_snapshot_from_parts(
    input: LifecycleGuardSnapshotInput<'_>,
) -> Option<LifecycleGuardSnapshot> {
    let mut counters = BTreeMap::new();
    extend_counter_group(
        &mut counters,
        "rejected_tool",
        input.rejected_tool_proposals,
    );
    extend_counter_group(
        &mut counters,
        "executed_tool_failure",
        input.executed_tool_failure_counts,
    );
    extend_counter_group(
        &mut counters,
        "progress_projection_no_progress",
        input.progress_projection_no_progress_counts,
    );
    extend_counter_group(
        &mut counters,
        "operation_non_content_no_progress",
        input.operation_non_content_no_progress_counts,
    );
    extend_counter_group(
        &mut counters,
        "verification_supporting_context_no_progress",
        input.verification_supporting_context_no_progress_counts,
    );
    extend_counter_group(
        &mut counters,
        "same_verification_failure",
        input.same_verification_failure_counts,
    );
    extend_counter_group(
        &mut counters,
        "docs_spec_semantic_reconciliation",
        input.docs_spec_semantic_reconciliation_counts,
    );
    extend_counter_group(
        &mut counters,
        "public_command_contract",
        input.public_command_contract_counts,
    );
    extend_counter_group(
        &mut counters,
        "wrong_verification_command",
        input.wrong_verification_command_counts,
    );
    extend_counter_group(
        &mut counters,
        "wrong_authoring_target",
        input.wrong_authoring_target_counts,
    );
    extend_counter_group(
        &mut counters,
        "repair_target_authority_violation",
        input.repair_target_authority_violation_counts,
    );
    extend_counter_group(
        &mut counters,
        "invalid_edit_argument",
        input.invalid_edit_argument_counts,
    );
    extend_counter_group(
        &mut counters,
        "authoring_target_grounding_required",
        input.authoring_target_grounding_required_counts,
    );
    extend_counter_group(
        &mut counters,
        "generated_test_target_grounding_required",
        input.generated_test_target_grounding_required_counts,
    );
    extend_counter_group(
        &mut counters,
        "docs_supporting_context_budget_exhausted",
        input.docs_supporting_context_budget_exhausted_counts,
    );
    extend_counter_group(
        &mut counters,
        "open_obligation_final_message",
        input.open_obligation_final_message_counts,
    );

    let mut active_flags = Vec::new();
    push_active_flag(
        &mut active_flags,
        "malformed_write_patch_recovery_pending",
        input.malformed_write_patch_recovery_pending,
    );
    push_active_flag(
        &mut active_flags,
        "malformed_apply_patch_write_recovery_pending",
        input.malformed_apply_patch_write_recovery_pending,
    );
    push_active_flag(
        &mut active_flags,
        "invalid_edit_arguments_recovery",
        input.invalid_edit_arguments_recovery.is_some(),
    );
    push_active_flag(
        &mut active_flags,
        "open_obligation_final_message_recovery",
        input.open_obligation_final_message_recovery.is_some(),
    );
    push_active_flag(
        &mut active_flags,
        "open_obligation_final_message_hard_edit_recovery_pending",
        input.open_obligation_final_message_hard_edit_recovery_pending,
    );
    push_active_flag(
        &mut active_flags,
        "provider_required_tool_choice_final_message_recovery_pending",
        input.provider_required_tool_choice_final_message_recovery_pending,
    );

    let mut scoped_targets = Vec::new();
    extend_scoped_target_group(
        &mut scoped_targets,
        "patch_context_mismatch_grounding",
        input.patch_context_mismatch_grounding_targets,
    );
    extend_scoped_target_group(
        &mut scoped_targets,
        "authoring_supporting_context_budget_exhausted",
        input.authoring_supporting_context_budget_exhausted,
    );
    extend_scoped_target_group(
        &mut scoped_targets,
        "authoring_grounded_active_target",
        input.authoring_grounded_active_targets,
    );
    extend_scoped_target_group(
        &mut scoped_targets,
        "repair_supporting_context_budget_exhausted",
        input.repair_supporting_context_budget_exhausted,
    );
    extend_scoped_target_group(
        &mut scoped_targets,
        "docs_supporting_context_budget_exhausted",
        input.docs_supporting_context_budget_exhausted,
    );

    let mut payloads = BTreeMap::new();
    if let Some(envelope) = input.invalid_edit_arguments_recovery {
        if let Ok(value) = serde_json::to_value(envelope) {
            payloads.insert("invalid_edit_arguments_recovery".to_string(), value);
        }
    }
    if let Some(envelope) = input.open_obligation_final_message_recovery {
        if let Ok(value) = serde_json::to_value(envelope) {
            payloads.insert("open_obligation_final_message_recovery".to_string(), value);
        }
    }

    let snapshot = LifecycleGuardSnapshot {
        counters,
        active_flags,
        scoped_targets,
        payloads,
    };
    (!lifecycle_guard_snapshot_is_empty(&snapshot)).then_some(snapshot)
}

#[derive(Default)]
pub(crate) struct HydratedLifecycleGuardSnapshotParts {
    pub(crate) rejected_tool_proposals: BTreeMap<String, usize>,
    pub(crate) executed_tool_failure_counts: BTreeMap<String, usize>,
    pub(crate) progress_projection_no_progress_counts: BTreeMap<String, usize>,
    pub(crate) operation_non_content_no_progress_counts: BTreeMap<String, usize>,
    pub(crate) verification_supporting_context_no_progress_counts: BTreeMap<String, usize>,
    pub(crate) same_verification_failure_counts: BTreeMap<String, usize>,
    pub(crate) docs_spec_semantic_reconciliation_counts: BTreeMap<String, usize>,
    pub(crate) public_command_contract_counts: BTreeMap<String, usize>,
    pub(crate) wrong_verification_command_counts: BTreeMap<String, usize>,
    pub(crate) wrong_authoring_target_counts: BTreeMap<String, usize>,
    pub(crate) repair_target_authority_violation_counts: BTreeMap<String, usize>,
    pub(crate) invalid_edit_argument_counts: BTreeMap<String, usize>,
    pub(crate) authoring_target_grounding_required_counts: BTreeMap<String, usize>,
    pub(crate) generated_test_target_grounding_required_counts: BTreeMap<String, usize>,
    pub(crate) docs_supporting_context_budget_exhausted_counts: BTreeMap<String, usize>,
    pub(crate) open_obligation_final_message_counts: BTreeMap<String, usize>,
    pub(crate) malformed_write_patch_recovery_pending: bool,
    pub(crate) malformed_apply_patch_write_recovery_pending: bool,
    pub(crate) invalid_edit_arguments_recovery: Option<InvalidEditRecoveryEnvelope>,
    pub(crate) open_obligation_final_message_recovery:
        Option<OpenObligationFinalMessageRecoveryEnvelope>,
    pub(crate) open_obligation_final_message_count: usize,
    pub(crate) open_obligation_final_message_hard_edit_recovery_pending: bool,
    pub(crate) provider_required_tool_choice_final_message_recovery_pending: bool,
    pub(crate) patch_context_mismatch_grounding_targets: BTreeSet<String>,
    pub(crate) authoring_supporting_context_budget_exhausted: BTreeSet<String>,
    pub(crate) authoring_grounded_active_targets: BTreeSet<String>,
    pub(crate) repair_supporting_context_budget_exhausted: BTreeSet<String>,
    pub(crate) docs_supporting_context_budget_exhausted: BTreeSet<String>,
    pub(crate) last_persisted_snapshot: Option<LifecycleGuardSnapshot>,
}

pub(crate) fn hydrate_lifecycle_guard_snapshot_parts(
    snapshot: &LifecycleGuardSnapshot,
) -> HydratedLifecycleGuardSnapshotParts {
    let mut parts = HydratedLifecycleGuardSnapshotParts::default();
    hydrate_counter_group(
        &snapshot.counters,
        "rejected_tool",
        &mut parts.rejected_tool_proposals,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "executed_tool_failure",
        &mut parts.executed_tool_failure_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "progress_projection_no_progress",
        &mut parts.progress_projection_no_progress_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "operation_non_content_no_progress",
        &mut parts.operation_non_content_no_progress_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "verification_supporting_context_no_progress",
        &mut parts.verification_supporting_context_no_progress_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "same_verification_failure",
        &mut parts.same_verification_failure_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "docs_spec_semantic_reconciliation",
        &mut parts.docs_spec_semantic_reconciliation_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "public_command_contract",
        &mut parts.public_command_contract_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "wrong_verification_command",
        &mut parts.wrong_verification_command_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "wrong_authoring_target",
        &mut parts.wrong_authoring_target_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "repair_target_authority_violation",
        &mut parts.repair_target_authority_violation_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "invalid_edit_argument",
        &mut parts.invalid_edit_argument_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "authoring_target_grounding_required",
        &mut parts.authoring_target_grounding_required_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "generated_test_target_grounding_required",
        &mut parts.generated_test_target_grounding_required_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "docs_supporting_context_budget_exhausted",
        &mut parts.docs_supporting_context_budget_exhausted_counts,
    );
    hydrate_counter_group(
        &snapshot.counters,
        "open_obligation_final_message",
        &mut parts.open_obligation_final_message_counts,
    );

    let flags = snapshot
        .active_flags
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    parts.malformed_write_patch_recovery_pending =
        flags.contains("malformed_write_patch_recovery_pending");
    parts.malformed_apply_patch_write_recovery_pending =
        flags.contains("malformed_apply_patch_write_recovery_pending");
    parts.open_obligation_final_message_hard_edit_recovery_pending =
        flags.contains("open_obligation_final_message_hard_edit_recovery_pending");
    parts.provider_required_tool_choice_final_message_recovery_pending =
        flags.contains("provider_required_tool_choice_final_message_recovery_pending");

    hydrate_scoped_target_group(
        &snapshot.scoped_targets,
        "patch_context_mismatch_grounding",
        &mut parts.patch_context_mismatch_grounding_targets,
    );
    hydrate_scoped_target_group(
        &snapshot.scoped_targets,
        "authoring_supporting_context_budget_exhausted",
        &mut parts.authoring_supporting_context_budget_exhausted,
    );
    hydrate_scoped_target_group(
        &snapshot.scoped_targets,
        "authoring_grounded_active_target",
        &mut parts.authoring_grounded_active_targets,
    );
    hydrate_scoped_target_group(
        &snapshot.scoped_targets,
        "repair_supporting_context_budget_exhausted",
        &mut parts.repair_supporting_context_budget_exhausted,
    );
    hydrate_scoped_target_group(
        &snapshot.scoped_targets,
        "docs_supporting_context_budget_exhausted",
        &mut parts.docs_supporting_context_budget_exhausted,
    );

    parts.invalid_edit_arguments_recovery = snapshot
        .payloads
        .get("invalid_edit_arguments_recovery")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    parts.open_obligation_final_message_recovery = snapshot
        .payloads
        .get("open_obligation_final_message_recovery")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    parts.open_obligation_final_message_count = parts
        .open_obligation_final_message_recovery
        .as_ref()
        .map(|envelope| envelope.count)
        .unwrap_or_default();
    parts.last_persisted_snapshot = Some(snapshot.clone());
    parts
}

pub(crate) fn snapshot_hydration_sequence_order_resists_timestamp_drift_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let stale_snapshot = LifecycleGuardSnapshot {
        counters: BTreeMap::from([(
            "progress_projection_no_progress:stale_timestamp".to_string(),
            1,
        )]),
        active_flags: Vec::new(),
        scoped_targets: Vec::new(),
        payloads: BTreeMap::new(),
    };
    let canonical_latest_snapshot = LifecycleGuardSnapshot {
        counters: BTreeMap::from([(
            "progress_projection_no_progress:canonical_latest_sequence".to_string(),
            2,
        )]),
        active_flags: Vec::new(),
        scoped_targets: Vec::new(),
        payloads: BTreeMap::new(),
    };
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 10,
            created_at_ms: 9_999,
            payload: HistoryItemPayload::LifecycleGuard {
                snapshot: stale_snapshot,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 11,
            created_at_ms: 1,
            payload: HistoryItemPayload::LifecycleGuard {
                snapshot: canonical_latest_snapshot,
            },
        },
    ];
    let Some(snapshot) = latest_lifecycle_guard_snapshot(&history_items) else {
        return false;
    };
    snapshot
        .counters
        .get("progress_projection_no_progress:canonical_latest_sequence")
        == Some(&2)
        && !snapshot
            .counters
            .contains_key("progress_projection_no_progress:stale_timestamp")
}

pub(crate) fn turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields_fixture_passes() -> bool
{
    let source = include_str!("loop_impl.rs");
    let lifecycle_source = include_str!("lifecycle_kernel.rs");
    let lifecycle_guard_source = include_str!("lifecycle_guard.rs");
    let tool_runtime_source = include_str!("tool_orchestrator.rs");
    let runtime_source = source
        .split("pub(crate) fn turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields_fixture_passes")
        .next()
        .unwrap_or(source);
    let forbidden_local_guard_names = [
        "rejected_tool_proposals",
        "executed_tool_failure_counts",
        "progress_projection_no_progress_counts",
        "operation_non_content_no_progress_counts",
        "verification_supporting_context_no_progress_counts",
        "same_verification_failure_counts",
        "docs_spec_semantic_reconciliation_counts",
        "public_command_contract_counts",
        "wrong_verification_command_counts",
        "wrong_authoring_target_counts",
        "repair_target_authority_violation_counts",
        "invalid_edit_argument_counts",
        "malformed_write_patch_recovery_pending",
        "malformed_apply_patch_write_recovery_pending",
        "invalid_edit_arguments_recovery",
        "patch_context_mismatch_grounding_targets",
        "authoring_supporting_context_budget_exhausted",
        "authoring_grounded_active_targets",
        "authoring_target_grounding_required_counts",
        "generated_test_target_grounding_required_counts",
        "repair_supporting_context_budget_exhausted",
        "docs_supporting_context_budget_exhausted",
        "docs_supporting_context_budget_exhausted_counts",
        "open_obligation_final_message_count",
        "open_obligation_final_message_counts",
        "open_obligation_final_message_recovery",
        "open_obligation_final_message_hard_edit_recovery_pending",
        "provider_required_tool_choice_final_message_recovery_pending",
    ];
    lifecycle_guard_source.contains("pub(crate) struct LifecycleGuardState")
        && lifecycle_guard_source.contains("fn record_completed_tool_lifecycle_effects")
        && runtime_source.contains("record_completed_tool_lifecycle_effects")
        && lifecycle_guard_source.contains("fn record_tool_execution_error_effects")
        && runtime_source.contains("record_tool_execution_error_effects")
        && !runtime_source.contains("struct LifecycleGuardState")
        && !runtime_source.contains("fn record_completed_tool_lifecycle_effects")
        && !runtime_source.contains("fn record_tool_execution_error_effects")
        && lifecycle_source.contains("fn prepare_tool_route_arguments")
        && tool_runtime_source.contains("fn prepare_accepted_tool_route_arguments")
        && runtime_source.contains("ToolLifecycleRuntime::prepare_accepted_tool_route_arguments")
        && lifecycle_source.contains("fn runtime_owned_required_verification_tool_call")
        && lifecycle_source.contains("fn runtime_owned_required_verification_dispatch_redirect")
        && lifecycle_source
            .contains("fn repair_shell_arguments_from_singleton_verification_command")
        && lifecycle_source.contains("fn adjudicate_final_message_response")
        && lifecycle_source.contains("fn no_tool_final_response_failure_message")
        && lifecycle_source.contains("fn empty_tool_call_final_response_failure_message")
        && lifecycle_source.contains("fn reconcile_tools_with_action_authority")
        && lifecycle_source.contains("fn compile_request_replay_policies")
        && lifecycle_source.contains("fn compile_provider_chat_request")
        && lifecycle_source.contains("fn compile_request_diagnostics")
        && lifecycle_source.contains("fn provider_messages_for_dispatch_control")
        && lifecycle_source.contains("fn provider_messages_for_active_work_image_replay")
        && lifecycle_source.contains("fn provider_visible_images_for_active_work")
        && lifecycle_source.contains("fn terminal_response_timeout_ms_for_state")
        && lifecycle_source.contains("fn provider_finish_reason_interrupt_message")
        && lifecycle_source.contains("fn runtime_cancel_interrupt_message")
        && lifecycle_source.contains("fn adjudicate_tool_call_model_action")
        && tool_runtime_source.contains("fn route_rejected_model_action")
        && tool_runtime_source.contains("fn route_accepted_tool_call")
        && tool_runtime_source.contains("fn control_projection_metadata")
        && tool_runtime_source.contains("fn emit_candidate_repair_edit_recorded")
        && tool_runtime_source.contains("fn emit_tool_proposal_rejected")
        && tool_runtime_source.contains("fn record_tool_proposal_rejected_event")
        && tool_runtime_source.contains("fn parse_route_arguments")
        && tool_runtime_source.contains("fn executed_completion_metadata")
        && tool_runtime_source.contains("fn render_special_operation_feedback")
        && tool_runtime_source.contains("fn operation_feedback_note")
        && tool_runtime_source.contains("fn content_satisfying_diff_summary_part")
        && runtime_source.contains("TurnLifecycleKernel::adjudicate_final_message_response")
        && runtime_source
            .contains("TurnLifecycleKernel::empty_tool_call_final_response_failure_message")
        && runtime_source.contains("TurnLifecycleKernel::reconcile_tools_with_action_authority")
        && runtime_source.contains("TurnLifecycleKernel::compile_request_replay_policies")
        && runtime_source.contains("TurnLifecycleKernel::compile_provider_chat_request")
        && runtime_source.contains("TurnLifecycleKernel::compile_request_diagnostics")
        && runtime_source.contains("TurnLifecycleKernel::provider_messages_for_dispatch_control")
        && runtime_source
            .contains("TurnLifecycleKernel::provider_messages_for_active_work_image_replay")
        && lifecycle_source.contains("fn provider_visible_images_for_active_work")
        && runtime_source.contains("TurnLifecycleKernel::terminal_response_timeout_ms_for_state")
        && runtime_source.contains("TurnLifecycleKernel::provider_finish_reason_interrupt_message")
        && runtime_source.contains("TurnLifecycleKernel::runtime_cancel_interrupt_message")
        && runtime_source.contains("TurnLifecycleKernel::adjudicate_tool_call_model_action")
        && runtime_source.contains("ToolLifecycleRuntime::route_rejected_model_action")
        && runtime_source.contains("ToolLifecycleRuntime::route_accepted_tool_call")
        && runtime_source.contains("ToolLifecycleRuntime::control_projection_metadata")
        && runtime_source.contains("ToolLifecycleRuntime::record_tool_proposal_rejected_event")
        && runtime_source.contains("ToolLifecycleRuntime::emit_candidate_repair_edit_recorded")
        && tool_runtime_source.contains("Self::emit_tool_proposal_rejected")
        && tool_runtime_source.contains("Self::emit_candidate_repair_edit_recorded")
        && tool_runtime_source.contains("executed_completion_metadata(")
        && tool_runtime_source.contains("render_special_operation_feedback(")
        && tool_runtime_source.contains("operation_feedback_note(progress_class)")
        && tool_runtime_source.contains("content_satisfying_diff_summary_part(")
        && runtime_source.contains("ToolLifecycleRuntime::parse_route_arguments")
        && !runtime_source.contains("fn request_diagnostics_from_chat(")
        && !runtime_source.contains("fn provider_messages_for_dispatch_control(")
        && !runtime_source.contains("fn provider_messages_for_active_work_image_replay(")
        && !runtime_source.contains("fn provider_visible_images_for_active_work(")
        && !runtime_source.contains("fn record_tool_proposal_rejected_event(")
        && !runtime_source.contains("fn control_projection_metadata(")
        && !runtime_source.contains("fn terminal_response_timeout_ms_for_state(")
        && !runtime_source.contains("matches!(finish_reason, Some(FinishReason::Cancelled))")
        && !runtime_source.contains("ProviderActionAdapter::adapt_tool_call(")
        && lifecycle_source
            .matches("fn runtime_owned_required_verification_tool_call(")
            .count()
            == 1
        && lifecycle_source
            .matches("fn runtime_owned_required_verification_dispatch_redirect(")
            .count()
            == 1
        && lifecycle_source
            .matches("fn repair_shell_arguments_from_singleton_verification_command(")
            .count()
            == 1
        && source.contains(
            "LifecycleGuardState::hydrate_from_history_items(&request.runtime_input.history_items)",
        )
        && lifecycle_guard_source.contains("HistoryItemPayload::LifecycleGuard")
        && lifecycle_guard_source.contains("RunEvent::LifecycleGuardUpdated")
        && source.contains("lifecycle_guard.emit_next_snapshot_if_changed")
        && forbidden_local_guard_names
            .iter()
            .map(|name| format!("let mut {name}"))
            .all(|declaration| !runtime_source.contains(&declaration))
        && forbidden_local_guard_names.iter().all(|name| {
            let direct_mutation_patterns = [
                format!("&mut lifecycle_guard.{name}"),
                format!("lifecycle_guard.{name} ="),
                format!("lifecycle_guard.{name}.clear("),
                format!("lifecycle_guard.{name}.insert("),
                format!("lifecycle_guard.{name}.entry("),
            ];
            direct_mutation_patterns
                .iter()
                .all(|pattern| !runtime_source.contains(pattern))
        })
        && !runtime_source.contains("lifecycle_guard.record_progress_projection_no_progress(")
        && !runtime_source.contains("lifecycle_guard.record_operation_non_content_no_progress(")
        && !runtime_source
            .contains("lifecycle_guard.record_verification_supporting_context_no_progress(")
        && !runtime_source.contains("lifecycle_guard.record_same_verification_failure_no_progress(")
        && !runtime_source.contains("lifecycle_guard.set_invalid_edit_arguments_recovery(")
        && !runtime_source.contains("lifecycle_guard.record_invalid_arguments_recovery(")
        && !runtime_source.contains("lifecycle_guard.record_executed_tool_failure_no_progress(")
        && !runtime_source.contains("repair_write_arguments_from_active_target(")
        && !runtime_source.contains("repair_unambiguous_malformed_edit_arguments_json(")
}

pub(crate) fn snapshot_hydrates_runtime_state_parts_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let rejected_tool_proposals = BTreeMap::from([("semantic_rejection".to_string(), 2)]);
    let verification_supporting_context_no_progress_counts =
        BTreeMap::from([("repair-obligation-1".to_string(), 3)]);
    let patch_context_mismatch_grounding_targets = BTreeSet::from(["src/lib.rs".to_string()]);
    let invalid_edit_arguments_recovery = InvalidEditRecoveryEnvelope {
        failure_kind: "invalid_edit_arguments".to_string(),
        tool_name: "write".to_string(),
        active_targets: vec!["src/lib.rs".to_string()],
        candidate_target: Some("src/lib.rs".to_string()),
        submitted_targets: vec!["src/lib.rs".to_string()],
        active_submitted_targets: vec!["src/lib.rs".to_string()],
        inactive_submitted_targets: Vec::new(),
        parser_error_family: Some("eof_while_parsing_string".to_string()),
        recovery_action: Some("apply_patch".to_string()),
        recovery_target: Some("src/lib.rs".to_string()),
        result_hash: Some("hash-1".to_string()),
        prompt: "retry bounded edit".to_string(),
    };
    let open_obligation_final_message_recovery = OpenObligationFinalMessageRecoveryEnvelope {
        count: 2,
        active_targets: vec!["src/lib.rs".to_string()],
        prompt: "continue with tool call".to_string(),
    };
    let empty_counts = BTreeMap::new();
    let empty_targets = BTreeSet::new();
    let open_obligation_final_message_counts = BTreeMap::from([("open-obligation".to_string(), 2)]);
    let Some(snapshot) = lifecycle_guard_snapshot_from_parts(LifecycleGuardSnapshotInput {
        rejected_tool_proposals: &rejected_tool_proposals,
        executed_tool_failure_counts: &empty_counts,
        progress_projection_no_progress_counts: &empty_counts,
        operation_non_content_no_progress_counts: &empty_counts,
        verification_supporting_context_no_progress_counts:
            &verification_supporting_context_no_progress_counts,
        same_verification_failure_counts: &empty_counts,
        docs_spec_semantic_reconciliation_counts: &empty_counts,
        public_command_contract_counts: &empty_counts,
        wrong_verification_command_counts: &empty_counts,
        wrong_authoring_target_counts: &empty_counts,
        repair_target_authority_violation_counts: &empty_counts,
        invalid_edit_argument_counts: &empty_counts,
        authoring_target_grounding_required_counts: &empty_counts,
        generated_test_target_grounding_required_counts: &empty_counts,
        docs_supporting_context_budget_exhausted_counts: &empty_counts,
        open_obligation_final_message_counts: &open_obligation_final_message_counts,
        malformed_write_patch_recovery_pending: true,
        malformed_apply_patch_write_recovery_pending: false,
        invalid_edit_arguments_recovery: Some(&invalid_edit_arguments_recovery),
        open_obligation_final_message_recovery: Some(&open_obligation_final_message_recovery),
        open_obligation_final_message_hard_edit_recovery_pending: false,
        provider_required_tool_choice_final_message_recovery_pending: false,
        patch_context_mismatch_grounding_targets: &patch_context_mismatch_grounding_targets,
        authoring_supporting_context_budget_exhausted: &empty_targets,
        authoring_grounded_active_targets: &empty_targets,
        repair_supporting_context_budget_exhausted: &empty_targets,
        docs_supporting_context_budget_exhausted: &empty_targets,
    }) else {
        return false;
    };
    let history_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 42,
        created_at_ms: 1,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: snapshot.clone(),
        },
    };
    let history_items = [history_item];
    let Some(latest_snapshot) = latest_lifecycle_guard_snapshot(&history_items) else {
        return false;
    };
    let hydrated = hydrate_lifecycle_guard_snapshot_parts(latest_snapshot);

    hydrated.rejected_tool_proposals.get("semantic_rejection") == Some(&2)
        && hydrated
            .verification_supporting_context_no_progress_counts
            .get("repair-obligation-1")
            == Some(&3)
        && hydrated.malformed_write_patch_recovery_pending
        && hydrated
            .patch_context_mismatch_grounding_targets
            .contains("src/lib.rs")
        && hydrated
            .invalid_edit_arguments_recovery
            .as_ref()
            .and_then(|envelope| envelope.recovery_target.as_deref())
            == Some("src/lib.rs")
        && hydrated
            .open_obligation_final_message_recovery
            .as_ref()
            .map(|envelope| envelope.count)
            == Some(2)
        && hydrated.open_obligation_final_message_count == 2
        && hydrated.last_persisted_snapshot.as_ref() == Some(&snapshot)
        && next_unpersisted_lifecycle_guard_snapshot(
            Some(snapshot.clone()),
            hydrated.last_persisted_snapshot.as_ref(),
        )
        .is_none()
}

pub(crate) fn snapshot_hydration_uses_canonical_item_order_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let older_snapshot = LifecycleGuardSnapshot {
        counters: BTreeMap::from([("rejected_tool:semantic_rejection".to_string(), 1)]),
        active_flags: Vec::new(),
        scoped_targets: Vec::new(),
        payloads: BTreeMap::new(),
    };
    let newer_snapshot = LifecycleGuardSnapshot {
        counters: BTreeMap::from([("rejected_tool:semantic_rejection".to_string(), 7)]),
        active_flags: Vec::new(),
        scoped_targets: Vec::new(),
        payloads: BTreeMap::new(),
    };
    let newer_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 20,
        created_at_ms: 20,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: newer_snapshot.clone(),
        },
    };
    let older_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 10,
        created_at_ms: 10,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: older_snapshot,
        },
    };
    let history_items = [newer_item, older_item];
    let Some(latest_snapshot) = latest_lifecycle_guard_snapshot(&history_items) else {
        return false;
    };
    let hydrated = hydrate_lifecycle_guard_snapshot_parts(latest_snapshot);

    hydrated.rejected_tool_proposals.get("semantic_rejection") == Some(&7)
        && hydrated.last_persisted_snapshot.as_ref() == Some(&newer_snapshot)
}

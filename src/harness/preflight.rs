use std::fs;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightLayer {
    Contract,
    Flow,
    HarnessReplay,
    QualityGate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightLlmMode {
    NoLlm,
    MockedLlm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightGateStatus {
    Active,
    BlockedPendingEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightGateFamily {
    ProtocolItemLifecycle,
    ControlEnvelopeProjection,
    StateReducerAuthority,
    PlanProgressProjectionAuthority,
    PromptReplayAuthority,
    ToolLifecycleAuthority,
    ToolProposalRejectionLifecycle,
    LlmTransportAuthority,
    VerificationEvidenceAuthority,
    ManualStEvidenceSchema,
    ArtifactReplaySchema,
    DesktopTranscriptProjectionAuthority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightResultStatus {
    Pass,
    Fail,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightGate {
    pub gate_id: String,
    pub purpose: String,
    pub tier: u8,
    pub layer: PreflightLayer,
    pub llm_mode: PreflightLlmMode,
    pub deterministic: bool,
    pub status: PreflightGateStatus,
    pub family: PreflightGateFamily,
    pub fixture_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightFixture {
    pub fixture_id: String,
    pub family: PreflightGateFamily,
    pub authority_source: String,
    #[serde(default)]
    pub required_refs: Vec<String>,
    #[serde(default)]
    pub forbidden_refs: Vec<String>,
    #[serde(default)]
    pub required_artifacts: Vec<String>,
    #[serde(default)]
    pub fail_closed_on_missing_typed_projection: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightGateReport {
    pub gate_id: String,
    pub fixture_id: Option<String>,
    pub layer: PreflightLayer,
    pub family: Option<PreflightGateFamily>,
    pub status: PreflightResultStatus,
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub status: PreflightResultStatus,
    pub results: Vec<PreflightGateReport>,
    pub generated_by: String,
}

impl PreflightReport {
    fn from_results(results: Vec<PreflightGateReport>) -> Self {
        let status = if results
            .iter()
            .any(|result| result.status == PreflightResultStatus::Fail)
        {
            PreflightResultStatus::Fail
        } else if results
            .iter()
            .any(|result| result.status == PreflightResultStatus::Blocked)
        {
            PreflightResultStatus::Blocked
        } else {
            PreflightResultStatus::Pass
        };
        Self {
            status,
            results,
            generated_by: "codex_style_preflight_v2".to_string(),
        }
    }
}

pub struct PreflightRunner;

impl PreflightRunner {
    pub fn run_active(gates: &[PreflightGate], fixtures: &[PreflightFixture]) -> PreflightReport {
        let results = gates
            .iter()
            .filter(|gate| gate.status == PreflightGateStatus::Active)
            .map(|gate| {
                let fixture = fixtures
                    .iter()
                    .find(|fixture| fixture.fixture_id == gate.fixture_id);
                match fixture {
                    Some(fixture) => evaluate_fixture(gate, fixture),
                    None => PreflightGateReport {
                        gate_id: gate.gate_id.clone(),
                        fixture_id: None,
                        layer: gate.layer,
                        family: Some(gate.family),
                        status: PreflightResultStatus::Blocked,
                        diagnostics: vec![format!("fixture `{}` is missing", gate.fixture_id)],
                        evidence_refs: Vec::new(),
                    },
                }
            })
            .collect();
        PreflightReport::from_results(results)
    }
}

fn evaluate_fixture(gate: &PreflightGate, fixture: &PreflightFixture) -> PreflightGateReport {
    let mut diagnostics = Vec::new();

    if gate.family != fixture.family {
        diagnostics.push(format!(
            "gate family {:?} does not match fixture family {:?}",
            gate.family, fixture.family
        ));
    }

    if !is_generic_primary_key(&gate.gate_id) {
        diagnostics.push(
            "gate id must be invariant/contract primary key, not manual ST case key".to_string(),
        );
    }

    if !fixture.forbidden_refs.is_empty() {
        let forbidden = fixture
            .forbidden_refs
            .iter()
            .any(|forbidden| fixture.authority_source.contains(forbidden));
        if forbidden {
            diagnostics.push(format!(
                "authority source `{}` contains forbidden legacy reference",
                fixture.authority_source
            ));
        }
    }

    for required in &fixture.required_refs {
        if !fixture.authority_source.contains(required) {
            diagnostics.push(format!(
                "authority source `{}` does not contain required typed reference `{required}`",
                fixture.authority_source
            ));
        }
    }

    if matches!(gate.family, PreflightGateFamily::ControlEnvelopeProjection)
        && !fixture.fail_closed_on_missing_typed_projection
    {
        diagnostics.push(
            "control envelope gate must fail closed when typed projection is missing".to_string(),
        );
    }

    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes()
    {
        diagnostics.push(
            "content-changing control projection text still conflates available tools with satisfying file-change progress".to_string(),
        );
    }

    if matches!(gate.family, PreflightGateFamily::ProtocolItemLifecycle)
        && !protocol_item_lifecycle_fixture_passes()
    {
        diagnostics.push(
            "canonical protocol item lifecycle does not preserve effective tool arguments, typed file change evidence, and typed tool output success".to_string(),
        );
    }

    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && (!protocol_persistence_unit_of_work_fixture_passes()
            || !crate::storage::session_repo::append_message_with_parts_uses_single_unit_of_work_fixture_passes()
            || !crate::app::run_service::resume_latest_user_message_uses_item_order_fixture_passes(
            )
            || !crate::agent::loop_impl::terminal_token_accounting_sequence_fixture_passes()
            || !crate::protocol::pre_recorded_protocol_sequence_reservation_fixture_passes())
    {
        diagnostics.push(
            "session storage can persist compatibility messages, message parts, session status, and protocol runtime projection through separate authorities, resume can select the latest user message by raw vector order instead of canonical item order, or pre-recorded protocol events can reuse a sequence number before the sink observes them"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.item_lifecycle.provider_replay_call_output_symmetry"
        && !provider_replay_call_output_symmetry_fixture_passes()
    {
        diagnostics.push(
            "provider replay is not built from canonical HistoryItem call/output pairs, or orphan/error items can still become assistant text".to_string(),
        );
    }

    if gate.gate_id == "preflight.llm_transport.stream_retry_before_first_event"
        && (!crate::llm::openai_compat::stream_event_retry_classifier_fixture_passes()
            || !crate::llm::openai_compat::stream_idle_timeout_retry_exhaustion_error_fixture_passes(
            )
            || !crate::llm::openai_compat::streaming_tool_call_projection_uses_delta_index_stable_ids_fixture_passes()
            || !crate::tui::prompt_enhance::prompt_enhance_sink_excludes_reasoning_delta_fixture_passes()
            || !crate::agent::loop_impl::request_diagnostics_stream_retry_policy_fixture_passes())
    {
        diagnostics.push(
            "provider SSE decode/transport failures or stream idle timeouts before the first emitted model event are not classified as retryable, retry exhaustion is not terminal evidence, streaming tool-call projection can split or collide call ids, reasoning deltas can become visible prompt text, request diagnostics omit stream retry policy, or non-transport/post-partial-output stream errors are retryable"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.llm_transport.streaming_timeout_boundary"
        && (!crate::llm::openai_compat::streaming_timeout_contract_fixture_passes()
            || !crate::agent::loop_impl::closeout_ready_final_response_timeout_guard_fixture_passes(
            ))
    {
        diagnostics.push(
            "provider streaming timeout contract does not separate response-header timeout from stream idle timeout"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && (!desktop_transcript_primary_reading_fixture_passes()
            || !crate::session::markdown::codex_turn_block_markdown_fixture_passes()
            || !crate::session::transcript::transcript_from_history_items_uses_item_sequence_fixture_passes()
            || !crate::cli::render::cli_history_renderer_uses_canonical_transcript_projection_fixture_passes()
            || !crate::tui::state::tui_turn_item_projection_uses_turn_local_sequence_fixture_passes()
            || !desktop_turn_item_projection_sequence_fixture_passes()
            || !desktop_file_change_projection_sequence_fixture_passes())
    {
        diagnostics.push(
            "Desktop/TUI/history Markdown projection does not preserve canonical item ordering, chronological turn blocks, turn-local item sequence, folded work/file-change evidence, and terminal outcome authority"
                .to_string(),
        );
    }

    if matches!(gate.family, PreflightGateFamily::StateReducerAuthority)
        && !state_reducer_runtime_feedback_fixture_passes()
    {
        diagnostics.push(
            "recoverable runtime feedback was classified as verification repair authority"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.state_reducer.requested_work_completion_promotes_verification" {
        let checks = [
            (
                "requested_work_completion_promotes_verification",
                crate::agent::state::requested_work_completion_promotes_verification_fixture_passes(
                ),
            ),
            (
                "required_verification_survives_authoring_completion",
                crate::agent::state::required_verification_survives_authoring_completion_fixture_passes(),
            ),
            (
                "partial_requested_work_remains_authoring_phase",
                crate::agent::state::partial_requested_work_remains_authoring_phase_fixture_passes(),
            ),
            (
                "passed_verification_consumes_pending_required_commands",
                crate::agent::state::passed_verification_consumes_pending_required_commands_fixture_passes(),
            ),
            (
                "resumed_new_user_turn_ignores_prior_closeout",
                crate::agent::state::resumed_new_user_turn_ignores_prior_closeout_fixture_passes(),
            ),
            (
                "new_authoring_turn_overrides_prior_verification",
                crate::agent::state::new_authoring_turn_overrides_prior_verification_fixture_passes(),
            ),
            (
                "partial_verification_pass_preserves_remaining_required_commands",
                crate::agent::state::partial_verification_pass_preserves_remaining_required_commands_fixture_passes(),
            ),
            (
                "reference_design_input_does_not_become_pending_authoring_target",
                crate::agent::state::reference_design_input_does_not_become_pending_authoring_target_fixture_passes(),
            ),
            (
                "scenario_contract_reference_input_does_not_become_authoring_target",
                crate::agent::state::scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes(),
            ),
            (
                "japanese_prompt_filename_boundaries_remain_artifact_targets",
                crate::agent::state::japanese_prompt_filename_boundaries_remain_artifact_targets_fixture_passes(),
            ),
            (
                "docs_output_referenced_code_does_not_become_pending_authoring_target",
                crate::agent::state::docs_output_referenced_code_does_not_become_pending_authoring_target_fixture_passes(),
            ),
            (
                "requested_work_without_verification_closes_after_file_change",
                crate::agent::state::requested_work_without_verification_closes_after_file_change_fixture_passes(),
            ),
            (
                "requested_work_relative_workspace_absolute_file_change_promotes_verification",
                crate::agent::state::requested_work_relative_workspace_absolute_file_change_promotes_verification_fixture_passes(),
            ),
            (
                "structured_document_summary_waits_for_remaining_sources",
                crate::agent::state::structured_document_summary_waits_for_remaining_sources_fixture_passes(),
            ),
            (
                "structured_document_summary_output_headings_survive_compacted_history",
                crate::agent::state::structured_document_summary_output_headings_survive_compacted_history_fixture_passes(),
            ),
            (
                "message_user_structured_document_progress",
                crate::agent::state::message_user_structured_document_progress_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "requested-work item-stream evidence did not preserve one or more invariants: {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.state_reducer.post_repair_edit_promotes_verification_rerun"
        && (!crate::agent::state::post_repair_file_change_promotes_verification_rerun_fixture_passes()
            || !crate::agent::turn_decision::post_repair_edit_progress_promotes_shell_rerun_fixture_passes()
            || !crate::agent::turn_decision::post_repair_verify_phase_ignores_stale_unclassified_repair_fixture_passes()
            || !crate::agent::loop_impl::post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes())
    {
        diagnostics.push(
            "successful repair edit progress can still leave stale repair edit authority instead of exact verification rerun".to_string(),
        );
    }

    if matches!(
        gate.family,
        PreflightGateFamily::PlanProgressProjectionAuthority
    ) && !plan_progress_projection_fixture_passes()
    {
        diagnostics.push(
            "progress projection still blocks known requested-work authoring authority".to_string(),
        );
    }

    let prompt_replay_failures =
        if matches!(gate.family, PreflightGateFamily::PromptReplayAuthority) {
            prompt_replay_stale_write_failed_fixtures()
        } else {
            Vec::new()
        };
    if !prompt_replay_failures.is_empty() {
        diagnostics.push(format!(
            "stale write payload would remain provider-visible as current action authority: {}",
            prompt_replay_failures.join(", ")
        ));
    }

    if gate.gate_id == "preflight.prompt_replay.active_user_hook_non_droppable"
        && !crate::agent::prompt::provider_replay_preserves_latest_user_across_trailing_compaction()
    {
        diagnostics.push(
            "provider replay can still treat a trailing compaction item as a hard boundary that drops the latest user/hook prompt".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::prompt::provider_replay_preserves_tool_pair_symmetry_with_model_arguments(
        )
    {
        diagnostics.push(
            "provider replay can still emit orphan tool outputs or skip replayable assistant tool calls when only model_arguments are populated".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::loop_impl::provider_replay_effective_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still include executable historical tool calls or tool outputs that are outside the current effective tool surface".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::loop_impl::provider_replay_omits_intermediate_assistant_text_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still expose unaccepted intermediate assistant text as authority while obligations remain open".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::lifecycle_kernel::provider_surface_filter_omits_orphan_assistant_prelude_fixture_passes()
    {
        diagnostics.push(
            "effective-surface provider replay can still keep assistant prelude text from an omitted historical tool-call item as standalone assistant authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::lifecycle_kernel::provider_surface_filter_omits_mixed_stale_assistant_prelude_fixture_passes()
    {
        diagnostics.push(
            "effective-surface provider replay can still keep assistant prelude text from a mixed historical tool-call item after omitting an out-of-surface call".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::prompt::provider_replay_preserves_current_invalid_edit_argument_feedback()
    {
        diagnostics.push(
            "provider replay can still drop current invalid_edit_arguments ToolOutput evidence when malformed edit arguments are not replayable".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::prompt::provider_replay_projects_rejected_final_message_evidence()
    {
        diagnostics.push(
            "provider replay can still drop rejected final-assistant-message lifecycle evidence instead of projecting it as typed no-progress context before the next dispatch".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::prompt::provider_replay_after_compaction_repairs_orphan_assistant_before_user(
        )
    {
        diagnostics.push(
            "provider replay can still emit an orphan assistant message after compaction without restoring its matching user query".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_trigger_ignores_pre_summary_history_fixture_passes(
        )
    {
        diagnostics.push(
            "compaction trigger still counts pre-summary history payloads after a compaction summary exists".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::llm_summary_text_is_wrapped_with_typed_continuity_fixture_passes()
    {
        diagnostics.push(
            "model-returned compaction summary text can still omit the typed CompactionContinuity marker or continuation focus".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::agent::prompt::stale_inactive_authoring_replay_omits_fake_executable_arguments()
    {
        diagnostics.push(
            "provider replay can still expose stale inactive authoring sentinel values as executable assistant tool-call arguments".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::agent::loop_impl::provider_system_context_normalization_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still emit runtime system context after a user message, which breaks OpenAI-compatible local chat-template tool continuation".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.progress_projection_pair_omitted"
        && !crate::agent::prompt::provider_replay_omits_stale_progress_projection_arguments()
    {
        diagnostics.push(
            "provider replay can still expose stale progress-projection todo JSON as executable assistant tool-call arguments or omit current call-id-scoped progress feedback".to_string(),
        );
    }

    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::openai_compatible_only_tool_policy_fixture_passes()
    {
        diagnostics.push(
            "OpenAI-compatible-only provider policy can still project final-answer-only behavior without preserving open-obligation tool lifecycle authority".to_string(),
        );
    }

    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::tool_call_turn_uses_configured_output_budget_fixture_passes()
    {
        diagnostics.push(
            "tool-enabled provider requests can still replace the configured model max_output_tokens with a separate tool-turn budget".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::payload_merges_provider_policy_and_runtime_system_control_fixture_passes()
    {
        diagnostics.push(
            "OpenAI-compatible chat payload can still split provider policy, runtime control projection, and recovery guidance across multiple system messages instead of one Codex-style instruction authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.lifecycle_kernel.provider_noncompliance_adjudication"
        && !crate::agent::lifecycle_kernel::provider_noncompliance_adjudication_fixture_passes()
    {
        diagnostics.push(
            "schema-outside or malformed provider tool proposals are not adjudicated into provider_noncompliance lifecycle evidence with shared ToolResult feedback, rejected proposal metadata, and semantic no-progress hash".to_string(),
        );
    }
    if gate.gate_id == "preflight.lifecycle_kernel.turn_lifecycle_plan_authority"
        && (!crate::agent::lifecycle_kernel::turn_lifecycle_plan_owns_dispatch_tool_choice_fixture_passes()
            || !crate::agent::lifecycle_kernel::provider_noncompliance_recovery_overrides_grounding_fixture_passes()
            || !crate::agent::lifecycle_kernel::wrong_target_authoring_recovery_hardens_active_target_fixture_passes())
    {
        diagnostics.push(
            "dispatch tool_choice, replay policy, proposal policy, corrective policy, terminal policy, continuation expectation, or diagnostics projection is still owned by TurnRuntime branch policy instead of a kernel-owned TurnLifecyclePlan with stable-surface and hard-recovery semantics".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.rejected_singleton_payload_terminal_guard"
        && !crate::agent::loop_impl::rejected_tool_batch_terminal_guard_waits_for_followup_fixture_passes()
    {
        diagnostics.push(
            "rejected tool proposals can still terminalize within the same provider response batch before the call-id-scoped corrective output is visible in a follow-up request".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.pre_execution_corrective_order_authority"
        && !crate::agent::tool_orchestrator::pre_execution_corrective_order_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "pre-execution corrective result ordering is not owned by ToolLifecycleRuntime, or repair probes can still be classified as wrong verification before repair target authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.no_content_write_is_no_progress"
        && (!crate::agent::tool_orchestrator::no_content_write_metadata_projects_no_progress_fixture_passes()
            || !crate::agent::tool_orchestrator::empty_file_change_is_not_authoring_progress_fixture_passes()
            || !crate::agent::loop_impl::invalid_edit_arguments_project_no_progress_recovery_fixture_passes()
            || !crate::agent::loop_impl::invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes()
            || !crate::agent::loop_impl::invalid_edit_recovery_projects_candidate_target_operation_fixture_passes()
            || !crate::agent::edit_recovery::invalid_edit_recovery_uses_open_target_when_candidate_is_inactive_fixture_passes()
            || !crate::agent::edit_recovery::mixed_target_apply_patch_preserves_active_hunk_evidence_fixture_passes()
            || !crate::agent::loop_impl::mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes()
            || !crate::agent::edit_recovery::apply_patch_context_mismatch_enters_invalid_edit_lifecycle_fixture_passes()
            || !crate::agent::loop_impl::invalid_edit_arguments_recovery_persists_across_final_message_fixture_passes()
            || !crate::agent::loop_impl::malformed_write_patch_capable_recovery_surface_fixture_passes()
            || !crate::agent::loop_impl::malformed_apply_patch_write_recovery_surface_fixture_passes()
            || !crate::agent::loop_impl::malformed_write_arguments_terminal_quote_repair_fixture_passes()
            || !crate::tool::apply_patch::destructive_noop_patch_is_rejected_fixture_passes()
            || !crate::tool::apply_patch::empty_or_zero_diff_patch_is_rejected_fixture_passes()
            || !crate::tool::apply_patch::hunkless_update_patch_is_rejected_fixture_passes()
            || !crate::tool::apply_patch::markdown_update_body_without_diff_prefix_is_rejected_fixture_passes()
            || !crate::edit::patch::patch_context_matching_is_exact_fixture_passes()
            || !crate::tool::apply_patch::add_file_unprefixed_content_line_feedback_names_line_fixture_passes())
    {
        diagnostics.push(
            "no-content write output, malformed edit argument feedback / patch-capable recovery surface, permissive patch context matching, or destructive no-op acknowledgement patch can still be projected without typed no-progress repair authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.active_authoring_rejects_wrong_target"
        && (!crate::agent::loop_impl::active_authoring_rejects_wrong_target_fixture_passes()
            || !crate::agent::loop_impl::verification_repair_rejects_non_exact_write_target_fixture_passes())
    {
        diagnostics.push(
            "requested-work authoring or verification repair still accepts content-changing writes outside the active deliverable / exact repair target set as progress".to_string(),
        );
    }

    if gate.gate_id == "preflight.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
        && !crate::agent::turn_decision::repair_required_active_work_ignores_shell_only_continuation_fixture_passes()
    {
        diagnostics.push(
            "repair-required active work can still be overridden by shell-only continuation or candidate surface".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.executed_failure_call_output_terminal_guard"
        && (!crate::agent::tool_orchestrator::executed_tool_failure_metadata_fixture_passes()
            || !crate::agent::loop_impl::executed_tool_failure_terminal_guard_fixture_passes()
            || !crate::agent::loop_impl::same_verification_failure_terminal_guard_fixture_passes())
    {
        diagnostics.push(
            "executed tool failures are not preserved as call-scoped failed outputs with stable no-progress terminal guard".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.verification_stable_tool_surface"
        && (!crate::agent::loop_impl::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes()
            || !crate::agent::loop_impl::repair_active_shell_probe_uses_repair_target_authority_fixture_passes()
            || !crate::agent::loop_impl::singleton_verification_command_arguments_are_runtime_owned_fixture_passes()
            || !crate::agent::loop_impl::verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes()
            || !crate::agent::state::public_verification_command_identity_dedupes_required_commands_fixture_passes()
            || !crate::protocol::verification_only_authority_narrows_to_exact_shell_fixture_passes())
    {
        diagnostics.push(
            "verification active work does not project exact shell verification authority, or repair-required verification lost its edit-capable recovery surface".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.authoring_stable_tool_surface"
        && !crate::agent::loop_impl::open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "requested-work authoring effective surface does not preserve file-changing tools while saturating plan-only progress projection after no-progress context".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.progress_projection_stable_surface_guard" {
        let checks = [
            (
                "open_authoring_operation_intent_preserves_tool_surface",
                crate::agent::loop_impl::open_authoring_operation_intent_preserves_tool_surface_fixture_passes(),
            ),
            (
                "progress_projection_recovery_narrows_to_edit_surface",
                crate::agent::lifecycle_kernel::progress_projection_recovery_narrows_to_edit_surface_fixture_passes(),
            ),
            (
                "progress_projection_recovery_preserves_target_grounding",
                crate::agent::lifecycle_kernel::progress_projection_recovery_preserves_target_grounding_fixture_passes(),
            ),
            (
                "docs_content_grounding_progress_projection_preserves_grounding_surface",
                crate::agent::lifecycle_kernel::docs_content_grounding_progress_projection_preserves_grounding_surface_fixture_passes(),
            ),
            (
                "docs_route_semantic_no_progress_guard",
                crate::agent::loop_impl::docs_route_semantic_no_progress_guard_fixture_passes(),
            ),
            (
                "docs_spec_semantic_reconciliation_no_progress_terminal_guard",
                crate::agent::tool_orchestrator::docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes(),
            ),
            (
                "docs_semantic_reconciliation_feedback_projection",
                crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_feedback_projection_fixture_passes(),
            ),
            (
                "docs_semantic_latest_user_sequence_authority",
                crate::agent::docs_semantic_contract::latest_user_authority_text_uses_sequence_order_fixture_passes(),
            ),
            (
                "authoring_supporting_context_budget_recovery_surface",
                crate::agent::loop_impl::authoring_supporting_context_budget_recovery_surface_fixture_passes(),
            ),
            (
                "multi_target_authoring_consumed_grounding_narrows_edit_recovery",
                crate::agent::loop_impl::multi_target_authoring_consumed_grounding_narrows_edit_recovery_fixture_passes(),
            ),
            (
                "repair_supporting_context_budget_recovery_surface",
                crate::agent::loop_impl::repair_supporting_context_budget_recovery_surface_fixture_passes(),
            ),
            (
                "invalid_edit_arguments_project_no_progress_recovery",
                crate::agent::loop_impl::invalid_edit_arguments_project_no_progress_recovery_fixture_passes(),
            ),
            (
                "invalid_edit_arguments_terminal_guard",
                crate::agent::loop_impl::invalid_edit_arguments_terminal_guard_fixture_passes(),
            ),
            (
                "failed_patch_context_mismatch_reopens_target_grounding",
                crate::agent::loop_impl::failed_patch_context_mismatch_reopens_target_grounding_fixture_passes(),
            ),
            (
                "docs_existing_target_update_keeps_exact_read_grounding",
                crate::agent::loop_impl::docs_existing_target_update_keeps_exact_read_grounding_fixture_passes(),
            ),
            (
                "verification_repair_target_grounding_surface_keeps_read",
                crate::agent::loop_impl::verification_repair_target_grounding_surface_keeps_read_fixture_passes(),
            ),
            (
                "source_repair_initial_grounding_precedes_edit_only_recovery",
                crate::agent::loop_impl::source_repair_initial_grounding_precedes_edit_only_recovery_fixture_passes(),
            ),
            (
                "generated_test_consumed_source_reference_requires_active_target",
                crate::agent::loop_impl::generated_test_consumed_source_reference_requires_active_target_fixture_passes(),
            ),
            (
                "singleton_missing_authoring_target_projects_create_action",
                crate::agent::loop_impl::singleton_missing_authoring_target_projects_create_action_fixture_passes(),
            ),
            (
                "code_authoring_final_message_recovery_reopens_stable_surface",
                crate::agent::loop_impl::code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes(),
            ),
            (
                "failed_edit_final_message_recovery_keeps_failed_edit_surface",
                crate::agent::loop_impl::failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes(),
            ),
            (
                "docs_route_supporting_context_budget_exhaustion_is_recoverable",
                crate::agent::loop_impl::docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes(),
            ),
            (
                "docs_route_reminder_projects_write_ready_boundary",
                crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "requested-work authoring fails to guard progress projection as call-scoped no-progress evidence, keeps plan-only progress tools after progress projection saturation, keeps supporting reads after all active targets are grounded, or repair supporting-context budget is not target-scoped; failed fixtures: {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.tool_lifecycle.edit_surface_registry_symmetry"
        && !crate::agent::loop_impl::edit_surface_registry_symmetry_fixture_passes()
    {
        diagnostics.push(
            "core edit tool surface and runtime dispatch registry can still diverge, or failed inactive write feedback is not preserved as a call-id-scoped ToolCall/ToolOutput pair".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"
        && (!crate::agent::tool_orchestrator::rejected_tool_semantic_terminal_guard_fixture_passes()
            || !crate::tool::registry::unknown_tool_feedback_does_not_restore_shell_surface_fixture_passes())
    {
        diagnostics.push(
            "rejected known-tool feedback still uses unstable argument/projection keys, fails to terminalize repeated disallowed or malformed proposals before outer timeout, or unknown-tool feedback can still restore a broad shell surface outside the active turn control envelope; provider noncompliance is verified by the lifecycle-kernel gate".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.workspace_relative_file_change_authority"
        && (!crate::workspace::project::path_separator_normalization_fixture_passes()
            || !crate::edit::change_path_storage_uses_workspace_relative_authority()
            || !crate::tool::search::glob_workspace_relative_pattern_fixture_passes())
    {
        diagnostics.push(
            "file-change lifecycle still stores route-root, absolute, or separator-drifted paths instead of workspace-relative authority, or glob matching/output still uses absolute host paths as model-visible authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.shell_mutation_syncs_edit_baseline"
        && (!crate::edit::safety::shell_mutation_syncs_confirmed_edit_baseline_fixture_passes()
            || !crate::tool::shell::shell_change_set_syncs_confirmed_edit_baseline_fixture_passes())
    {
        diagnostics.push(
            "shell-detected workspace file mutations can still bypass the confirmed-content baseline used by write/apply_patch stale-change guards".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.shell_output_encoding_authority"
        && !crate::tool::shell::shell_output_encoding_fixture_passes()
    {
        diagnostics.push(
            "shell stdout/stderr display projection can still mojibake Japanese Windows output or fails to force Python subprocess UTF-8 text mode".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.command_text_encoding_contract"
        && !crate::tool::shell::command_text_encoding_contract_fixture_passes()
    {
        diagnostics.push(
            "shell command text encoding review still allows text-producing verification commands to rely on platform defaults or hidden tool-owned UTF-8 bootstrap".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.shell_timeout_process_tree_authority"
        && !crate::tool::shell::shell_timeout_process_tree_termination_order_fixture_passes()
    {
        diagnostics.push(
            "shell timeout/cancellation can still terminate the parent shell before the descendant process tree, leaving orphaned child processes and stale pending tool lifecycle evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.shell_timeout_process_tree_authority"
        && !crate::tool::shell::shell_completion_process_tree_cleanup_fixture_passes()
    {
        diagnostics.push(
            "shell normal completion can still join captured pipes before cleaning descendant processes, leaving child processes alive after the parent command exits".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.closed_network_shell_authority"
        && (!crate::tool::shell::external_connection_shell_review_fixture_passes()
            || !crate::agent::prompt_assets::external_connection_prompt_projects_review_fixture_passes()
            || !crate::tool::shell::shell_output_projection_fixture_passes())
    {
        diagnostics.push(
            "shell tool does not require user review for external-connection or environment-setup commands, or stdout/stderr/exit-code recovery evidence is not projected to the UI/runtime item stream".to_string(),
        );
    }

    if gate.gate_id == "preflight.vision.input_item_lifecycle_authority"
        && (!crate::agent::prompt::vision_input_provider_projection_fixture_passes()
            || !crate::harness::manual_st::vision_prompt_uses_labeled_attachment_fixture_passes())
    {
        diagnostics.push(
            "vision input items are not projected as Codex-style labeled image content, or diagnostic source paths still leak into provider-visible workspace authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.workspace.absolute_turn_cwd_root_authority"
        && (!crate::workspace::discovery::workspace_discovery_absolute_root_authority_fixture_passes()
            || !crate::workspace::path_guard::path_guard_rejects_cross_workspace_absolute_remap_fixture_passes())
    {
        diagnostics.push(
            "workspace discovery can still produce an empty or relative turn cwd/root authority, or path guard can still remap a foreign absolute path into the active workspace by matching a directory name"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.turn_decision.codex_stable_tool_surface_authority" {
        let checks = [
            (
                "singleton_write_surface_requires_tool_choice",
                crate::agent::loop_impl::singleton_write_surface_requires_tool_choice_fixture_passes(
                ),
            ),
            (
                "concrete_write_required_action_narrows_broad_surface",
                crate::agent::loop_impl::concrete_write_required_action_narrows_broad_surface_fixture_passes(),
            ),
            (
                "codex_style_code_authoring_omits_whole_file_write",
                crate::agent::loop_impl::codex_style_code_authoring_omits_whole_file_write_fixture_passes(),
            ),
            (
                "codex_style_code_authoring_omits_json_discovery_surface",
                crate::agent::loop_impl::codex_style_code_authoring_omits_json_discovery_surface_fixture_passes(),
            ),
            (
                "codex_style_docs_authoring_omits_non_codex_json_surface",
                crate::agent::loop_impl::codex_style_docs_authoring_omits_non_codex_json_surface_fixture_passes(),
            ),
            (
                "open_work_uses_auto_tool_choice_with_harness_closeout_guard",
                crate::agent::loop_impl::open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes(),
            ),
            (
                "open_obligation_final_message_recovery_preserves_stable_surface",
                crate::agent::loop_impl::open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes(),
            ),
            (
                "failed_edit_final_message_recovery_keeps_failed_edit_surface",
                crate::agent::loop_impl::failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes(),
            ),
            (
                "provider_required_tool_choice_final_message_recovery",
                crate::agent::loop_impl::provider_required_tool_choice_final_message_recovery_fixture_passes(),
            ),
            (
                "multi_target_open_authoring_final_message_correction_names_targets",
                crate::agent::loop_impl::multi_target_open_authoring_final_message_correction_names_targets_fixture_passes(),
            ),
            (
                "final_message_recovery_is_system_control_projection",
                crate::agent::loop_impl::final_message_recovery_is_system_control_projection_fixture_passes(),
            ),
            (
                "open_obligation_final_message_recovery_persists_across_no_progress_tool",
                crate::agent::loop_impl::open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes(),
            ),
            (
                "authoring_final_message_recovery_keeps_target_grounding_read",
                crate::agent::loop_impl::authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes(),
            ),
            (
                "docs_patch_context_final_message_recovery_preserves_grounding",
                crate::agent::loop_impl::docs_patch_context_final_message_recovery_preserves_grounding_fixture_passes(),
            ),
            (
                "docs_existing_target_update_keeps_exact_read_grounding",
                crate::agent::loop_impl::docs_existing_target_update_keeps_exact_read_grounding_fixture_passes(),
            ),
            (
                "generated_test_authoring_keeps_recent_source_reference_read",
                crate::agent::loop_impl::generated_test_authoring_keeps_recent_source_reference_read_fixture_passes(),
            ),
            (
                "generated_test_consumed_source_reference_requires_active_target",
                crate::agent::loop_impl::generated_test_consumed_source_reference_requires_active_target_fixture_passes(),
            ),
            (
                "source_repair_final_message_correction_uses_exact_write_action",
                crate::agent::loop_impl::source_repair_final_message_correction_uses_exact_write_action_fixture_passes(),
            ),
            (
                "open_obligation_final_message_guard",
                crate::agent::loop_impl::open_obligation_final_message_guard_fixture_passes(),
            ),
            (
                "open_obligation_final_message_guard_is_recovery_context_keyed",
                crate::agent::loop_impl::open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes(),
            ),
            (
                "singleton_active_target_write_arguments_repair",
                crate::agent::loop_impl::singleton_active_target_write_arguments_repair_fixture_passes(),
            ),
            (
                "singleton_missing_target_stable_surface_projects_apply_patch_action",
                crate::protocol::singleton_missing_target_stable_surface_projects_apply_patch_action_fixture_passes(),
            ),
            (
                "repair_target_identity_aliases_compile_exact_write_action",
                crate::protocol::repair_target_identity_aliases_compile_exact_write_action_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "Codex stable tool surface authority failed invariant(s): {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.turn_decision.active_work_edit_before_verification_rerun"
        && (!crate::agent::state::verification_failure_promotes_repair_required_active_work_fixture_passes()
            || !crate::agent::state::source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes()
            || !crate::agent::state::source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes()
            || !crate::agent::state::contract_visible_public_exception_active_work_targets_source_fixture_passes()
            || !crate::agent::state::generated_test_validity_active_work_outranks_source_sibling_fixture_passes()
            || !crate::agent::state::mixed_source_public_api_and_generated_test_name_resolution_active_work_fixture_passes()
            || !crate::agent::state::generated_test_parse_defect_active_work_matches_repair_lane_fixture_passes()
            || !crate::agent::state::generated_test_api_misuse_active_work_targets_test_fixture_passes()
            || !crate::agent::state::generated_test_module_attribute_api_misuse_active_work_targets_test_fixture_passes()
            || !crate::agent::state::generated_test_exception_type_overreach_active_work_targets_test_fixture_passes()
            || !crate::agent::state::no_tests_ran_recent_generated_test_filechange_preserves_target_fixture_passes()
            || !crate::agent::state::generated_test_local_binding_contradiction_active_work_fixture_passes()
            || !crate::agent::state::post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes()
            || !crate::agent::repair_lane::no_tests_ran_missing_generated_test_target_stays_test_owned_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_stays_diagnostic_fixture_passes()
            || !crate::agent::repair_lane::generated_test_parse_defect_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_subprocess_encoding_missing_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_import_nameerror_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_reflection_api_misuse_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_module_attribute_api_misuse_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::repair_intent_defers_verification_command_evidence_fixture_passes()
            || !crate::agent::repair_lane::generated_test_contract_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::contract_visible_public_exception_projects_source_repair_fixture_passes()
            || !crate::agent::repair_lane::generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes()
            || !crate::agent::repair_lane::ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_exception_type_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::turn_decision::active_work_edit_authority_precedes_verification_rerun_fixture_passes()
            || !crate::agent::turn_decision::repair_lane_target_matches_active_work_authority_fixture_passes()
            || !crate::agent::loop_impl::required_repair_write_missing_tool_is_not_restored_fixture_passes()
            || !crate::agent::loop_impl::failed_patch_context_mismatch_reopens_target_grounding_fixture_passes()
            || !crate::agent::loop_impl::verification_repair_target_grounding_surface_keeps_read_fixture_passes())
    {
        let mut failed_fixtures = Vec::new();
        if !crate::agent::state::verification_failure_promotes_repair_required_active_work_fixture_passes() {
            failed_fixtures.push("verification_failure_promotes_repair_required_active_work");
        }
        if !crate::agent::state::source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes() {
            failed_fixtures.push("source_owned_repair_active_work_excludes_generated_test_evidence");
        }
        if !crate::agent::state::source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes() {
            failed_fixtures
                .push("source_owned_requirement_refs_align_active_work_with_repair_lane");
        }
        if !crate::agent::state::contract_visible_public_exception_active_work_targets_source_fixture_passes() {
            failed_fixtures
                .push("contract_visible_public_exception_active_work_targets_source");
        }
        if !crate::agent::state::generated_test_validity_active_work_outranks_source_sibling_fixture_passes() {
            failed_fixtures.push("generated_test_validity_active_work_outranks_source_sibling");
        }
        if !crate::agent::state::mixed_source_public_api_and_generated_test_name_resolution_active_work_fixture_passes() {
            failed_fixtures.push("mixed_source_public_api_and_generated_test_name_resolution_active_work");
        }
        if !crate::agent::state::generated_test_parse_defect_active_work_matches_repair_lane_fixture_passes() {
            failed_fixtures.push("generated_test_parse_defect_active_work_matches_repair_lane");
        }
        if !crate::agent::state::generated_test_api_misuse_active_work_targets_test_fixture_passes()
        {
            failed_fixtures.push("generated_test_api_misuse_active_work_targets_test");
        }
        if !crate::agent::state::generated_test_module_attribute_api_misuse_active_work_targets_test_fixture_passes()
        {
            failed_fixtures
                .push("generated_test_module_attribute_api_misuse_active_work_targets_test");
        }
        if !crate::agent::state::generated_test_exception_type_overreach_active_work_targets_test_fixture_passes() {
            failed_fixtures
                .push("generated_test_exception_type_overreach_active_work_targets_test");
        }
        if !crate::agent::state::no_tests_ran_recent_generated_test_filechange_preserves_target_fixture_passes() {
            failed_fixtures.push("no_tests_ran_recent_generated_test_filechange_preserves_target");
        }
        if !crate::agent::state::generated_test_local_binding_contradiction_active_work_fixture_passes() {
            failed_fixtures.push("generated_test_local_binding_contradiction_active_work");
        }
        if !crate::agent::state::post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes() {
            failed_fixtures.push("post_repair_generated_test_public_output_overreach_enters_test_repair");
        }
        if !crate::agent::repair_lane::no_tests_ran_missing_generated_test_target_stays_test_owned_fixture_passes() {
            failed_fixtures.push("no_tests_ran_missing_generated_test_target_stays_test_owned");
        }
        if !crate::agent::repair_lane::source_owned_repair_lane_stays_diagnostic_fixture_passes() {
            failed_fixtures.push("source_owned_repair_lane_stays_diagnostic");
        }
        if !crate::agent::repair_lane::generated_test_parse_defect_projects_test_repair_fixture_passes() {
            failed_fixtures.push("generated_test_parse_defect_projects_test_repair");
        }
        if !crate::agent::repair_lane::generated_test_subprocess_encoding_missing_projects_test_repair_fixture_passes() {
            failed_fixtures
                .push("generated_test_subprocess_encoding_missing_projects_test_repair");
        }
        if !crate::agent::repair_lane::generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes() {
            failed_fixtures.push(
                "generated_test_subprocess_output_capture_missing_projects_test_repair",
            );
        }
        if !crate::agent::repair_lane::generated_test_import_nameerror_projects_test_repair_fixture_passes() {
            failed_fixtures.push("generated_test_import_nameerror_projects_test_repair");
        }
        if !crate::agent::repair_lane::generated_test_reflection_api_misuse_projects_test_repair_fixture_passes() {
            failed_fixtures.push("generated_test_reflection_api_misuse_projects_test_repair");
        }
        if !crate::agent::repair_lane::generated_test_module_attribute_api_misuse_projects_test_repair_fixture_passes() {
            failed_fixtures.push(
                "generated_test_module_attribute_api_misuse_projects_test_repair",
            );
        }
        if !crate::agent::repair_lane::repair_intent_defers_verification_command_evidence_fixture_passes() {
            failed_fixtures.push("repair_intent_defers_verification_command_evidence");
        }
        if !crate::agent::repair_lane::generated_test_contract_overreach_projects_test_repair_fixture_passes() {
            failed_fixtures.push("generated_test_contract_overreach_projects_test_repair");
        }
        if !crate::agent::repair_lane::contract_visible_public_exception_projects_source_repair_fixture_passes() {
            failed_fixtures
                .push("contract_visible_public_exception_projects_source_repair");
        }
        if !crate::agent::repair_lane::generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes() {
            failed_fixtures
                .push("generic_generated_test_only_repair_lane_preserves_active_test_target");
        }
        if !crate::agent::repair_lane::ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes() {
            failed_fixtures
                .push("ungrounded_generated_public_output_assertion_projects_test_repair");
        }
        if !crate::agent::repair_lane::generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes() {
            failed_fixtures.push(
                "generated_test_public_output_numeric_format_overreach_projects_test_repair",
            );
        }
        if !crate::agent::repair_lane::generated_test_exception_type_overreach_projects_test_repair_fixture_passes() {
            failed_fixtures
                .push("generated_test_exception_type_overreach_projects_test_repair");
        }
        if !crate::agent::turn_decision::active_work_edit_authority_precedes_verification_rerun_fixture_passes() {
            failed_fixtures.push("active_work_edit_authority_precedes_verification_rerun");
        }
        if !crate::agent::turn_decision::repair_lane_target_matches_active_work_authority_fixture_passes() {
            failed_fixtures.push("repair_lane_target_matches_active_work_authority");
        }
        if !crate::agent::loop_impl::required_repair_write_missing_tool_is_not_restored_fixture_passes() {
            failed_fixtures.push("required_repair_write_missing_tool_is_not_restored");
        }
        if !crate::agent::loop_impl::failed_patch_context_mismatch_reopens_target_grounding_fixture_passes() {
            failed_fixtures.push("failed_patch_context_mismatch_reopens_target_grounding");
        }
        if !crate::agent::loop_impl::verification_repair_target_grounding_surface_keeps_read_fixture_passes() {
            failed_fixtures.push("verification_repair_target_grounding_surface_keeps_read");
        }
        diagnostics.push(
            format!(
                "source-owned verification repair is not compiled through item-stream active work / ActionAuthority, or repair-lane diagnostics still override the top-level dispatch authority; failed fixtures: {}",
                failed_fixtures.join(", ")
            ),
        );
    }

    if gate.gate_id == "preflight.closeout.final_assistant_message_lifecycle"
        && (!crate::agent::loop_impl::clean_closeout_final_message_lifecycle_fixture_passes()
            || !crate::agent::loop_impl::answer_only_final_message_lifecycle_fixture_passes()
            || !crate::agent::loop_impl::closeout_ready_final_response_timeout_guard_fixture_passes(
            )
            || !crate::agent::loop_impl::invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes())
    {
        diagnostics.push(
            "clean closeout still requires a synthetic completion tool, no-executable-work answer-only turns can still reject final assistant messages, closeout can wait indefinitely for a provider final message after item-stream evidence is already satisfied, or an invalid-tool recovery shell success can still synthesize assistant text and complete without final assistant lifecycle authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.open_obligation_final_assistant_continuation_hook"
        && (!crate::harness::manual_st::final_assistant_open_obligation_not_clean_closeout_fixture_passes()
            || !crate::harness::manual_st::final_assistant_open_obligation_continuation_hook_fixture_passes()
            || !crate::harness::manual_st::open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes()
            || !crate::harness::manual_st::route_verification_waits_for_authored_artifacts_fixture_passes()
            || !crate::harness::manual_st::closeout_continuation_is_text_only_fixture_passes()
            || !crate::harness::manual_st::stage_scoped_closeout_evidence_is_invalidated_fixture_passes()
            || !crate::harness::manual_st::latest_verification_result_drives_closeout_fixture_passes()
            || !crate::harness::manual_st::verification_evidence_after_content_change_invalidated_fixture_passes()
            || !crate::harness::manual_st::stage_without_required_verification_ignores_prior_stale_verification_fixture_passes()
            || !crate::harness::manual_st::runtime_verification_pass_after_content_change_satisfies_route_closeout_fixture_passes()
            || !crate::harness::manual_st::runtime_failure_closeout_recomputes_current_artifacts_fixture_passes()
            || !crate::harness::manual_st::run_error_closeout_replaces_stale_continuation_evidence_fixture_passes()
            || !crate::harness::manual_st::run_error_open_obligation_uses_closeout_continuation_budget_fixture_passes()
            || !crate::harness::manual_st::runtime_terminal_status_uses_closeout_continuation_budget_fixture_passes()
            || !crate::harness::manual_st::closeout_continuation_budget_blocks_same_workspace_stall_fixture_passes()
            || !crate::harness::manual_st::successful_closeout_continuation_rematerializes_case_verdict_fixture_passes()
            || !crate::harness::manual_st::route_terminal_verdict_rematerializes_from_case_results_fixture_passes()
            || !crate::harness::manual_st::completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes()
            || !crate::harness::manual_st::satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes()
            || !crate::agent::state::manual_st_closeout_expected_artifacts_inventory_does_not_reopen_fixture_passes())
    {
        diagnostics.push(
            "runtime-completed, runtime-error, or runtime-terminal final assistant messages with open obligations are not converted into explicit text-only continuation user-turn items, expected artifact inventory can reopen non-stage authoring targets, current workspace artifacts fail to clear stale authoring obligations, satisfied docs repair can reopen route closeout, route verification can run before authored artifacts exist, closeout evidence can leak across stages, closeout verification does not use latest command evidence, verification pass evidence remains fresh after later content changes, runtime failures can keep stale missing-artifact closeout evidence, runtime open obligations bypass the closeout continuation budget, same-workspace no-progress continuations are not bounded, a successful closeout continuation does not re-materialize the case verdict from latest terminal evidence, or route-level verdict/stop_reason is not re-materialized from current case results".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.verification_failure_preserves_closeout_evidence"
        && !crate::harness::manual_st::verification_failure_preserves_closeout_evidence_fixture_passes()
    {
        diagnostics.push(
            "route verification failure can still short-circuit before ManualStCloseoutEvidence materializes open obligations, missing artifacts, and failed verification together".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.verification_repair_continuation_hook"
        && (!crate::harness::manual_st::verification_failed_closeout_builds_repair_hook_prompt_fixture_passes()
            || !crate::harness::manual_st::verification_failed_closeout_uses_generated_test_parse_target_fixture_passes()
            || !crate::harness::manual_st::closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes())
    {
        diagnostics.push(
            "failed verification closeout does not project a Codex-style verification-repair hook prompt, generated-test parse defects can still fall back to source repair targets, or closeout continuation budget is still scoped to the whole stage instead of the failure signature".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.verification_labels_not_requested_work"
        && (!crate::agent::state::verification_failure_labels_are_not_requested_work_targets_fixture_passes()
            || !crate::agent::state::verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes()
            || !crate::agent::state::continuation_context_symbols_are_not_requested_work_targets_fixture_passes()
            || !crate::harness::manual_st::verification_failure_labels_do_not_become_authoring_obligations_fixture_passes())
    {
        diagnostics.push(
            "failed verification labels, traceback paths, or test method symbols can still become requested-work authoring targets, or failed verification closeout can still lose its edit-capable repair hook lifecycle".to_string(),
        );
    }

    if gate.gate_id == "preflight.route_evidence.schema"
        && (!crate::harness::manual_st::multistage_continuation_uses_explicit_session_without_continue_last_fixture_passes()
            || !crate::harness::manual_st::provider_stream_idle_timeout_classification_fixture_passes()
            || !crate::harness::manual_st::provider_transport_stream_error_classification_fixture_passes()
            || !crate::harness::manual_st::semantic_no_progress_terminal_classification_fixture_passes()
            || !crate::harness::manual_st::route_owned_command_timeout_fixture_passes()
            || !crate::harness::manual_st::route_evidence_filters_generated_dependency_paths_fixture_passes()
            || !crate::harness::manual_st::route_evidence_overwrites_stale_timeout_classification_fixture_passes()
            || !crate::harness::manual_st::route_result_progress_fields_fixture_passes()
            || !crate::harness::manual_st::route_inflight_case_progress_artifact_fixture_passes()
            || !crate::harness::manual_st::route_case_progress_phase_boundaries_fixture_passes()
            || !crate::harness::manual_st::stage_scoped_verification_commands_are_spec_owned_fixture_passes()
            || !crate::harness::manual_st::manual_st_visible_scenario_contract_prompt_fixture_passes())
    {
        diagnostics.push(
            "manual ST route evidence can still lose explicit session continuation, in-flight case progress phase boundaries, provider stream timeout classification, route-owned command timeout/wait policy, stage-scoped verification command ownership, fresh output-root ownership, bounded workspace manifest filtering, or prompt-visible scenario contract authority".to_string(),
        );
    }

    if gate.gate_id
        == "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
        && (!crate::agent::state::verification_failure_preserves_repair_targets_fixture_passes()
            || !crate::agent::state::source_owned_verification_failure_preserves_recent_source_edit_target_fixture_passes()
            || !crate::agent::state::verification_timeout_preserves_recent_source_repair_target_fixture_passes()
            || !crate::agent::state::out_of_order_history_items_use_sequence_authority_for_repair_fixture_passes()
            || !crate::agent::state::verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes()
            || !crate::agent::state::verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes()
            || !crate::agent::state::continuation_context_symbols_are_not_requested_work_targets_fixture_passes()
            || !crate::agent::state::verification_repair_continuation_projects_repair_state_fixture_passes()
            || !crate::agent::state::public_command_contract_continuation_projects_compact_source_repair_fixture_passes()
            || !crate::agent::state::verification_repair_continuation_generated_test_parse_target_fixture_passes()
            || !crate::agent::state::verification_failure_ignores_runtime_loader_frame_fixture_passes()
            || !crate::agent::state::verification_repair_targets_from_state_ignore_diagnostic_scalars_fixture_passes()
            || !crate::agent::state::message_user_protected_reference_filters_verification_targets_fixture_passes()
            || !crate::agent::state::public_output_stream_source_repair_active_work_uses_source_target_fixture_passes()
            || !crate::agent::state::source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes()
            || !crate::agent::state::source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes()
            || !crate::agent::state::contract_visible_public_exception_active_work_targets_source_fixture_passes()
            || !crate::agent::state::generated_test_validity_active_work_outranks_source_sibling_fixture_passes()
            || !crate::agent::state::generated_test_parse_defect_active_work_matches_repair_lane_fixture_passes()
            || !crate::agent::state::generated_test_api_misuse_active_work_targets_test_fixture_passes()
            || !crate::agent::state::generated_test_module_attribute_api_misuse_active_work_targets_test_fixture_passes()
            || !crate::agent::state::generated_test_exception_type_overreach_active_work_targets_test_fixture_passes()
            || !crate::agent::state::generated_test_local_binding_contradiction_active_work_fixture_passes()
            || !crate::agent::state::post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes()
            || !crate::agent::loop_impl::operation_feedback_uses_active_work_targets_fixture_passes()
            || !crate::agent::repair_lane::source_owned_verification_repair_lane_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_rejects_diagnostic_label_targets_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_derives_source_from_generated_test_target_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_canonicalizes_absolute_source_target_fixture_passes()
            || !crate::agent::repair_lane::public_output_stream_assertion_mismatch_fixture_passes()
            || !crate::agent::repair_lane::generated_test_parse_defect_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_subprocess_encoding_missing_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_import_nameerror_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_reflection_api_misuse_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_module_attribute_api_misuse_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::repair_intent_defers_verification_command_evidence_fixture_passes()
            || !crate::agent::repair_lane::public_command_contract_failure_projects_compact_source_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_contract_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::contract_visible_public_exception_projects_source_repair_fixture_passes()
            || !crate::agent::repair_lane::generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes()
            || !crate::agent::repair_lane::ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::repair_lane::generated_test_exception_type_overreach_projects_test_repair_fixture_passes()
            || !crate::agent::turn_decision::unclassified_repair_fails_closed_before_dispatch_fixture_passes()
            || !crate::agent::turn_decision::repair_lane_target_matches_active_work_authority_fixture_passes()
            || !crate::agent::contract_reconciliation::contract_reconciliation_ignores_diagnostic_label_targets_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_constructor_misuse_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_parse_defect_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::source_parse_defect_is_source_owned_without_requirement_id_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_name_resolution_self_defect_without_source_public_api_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_api_misuse_without_source_public_api_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::mixed_generated_test_name_resolution_source_public_api_is_source_test_mismatch_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_local_binding_contradiction_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::generic_generated_test_only_failure_preserves_active_test_target_fixture_passes()
            || !crate::agent::contract_reconciliation::contract_visible_public_exception_failure_is_source_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::generated_test_exception_type_overreach_is_test_owned_fixture_passes()
            || !crate::agent::contract_reconciliation::mixed_generated_test_validity_and_public_behavior_preserves_source_test_mismatch_fixture_passes()
            || !crate::agent::contract_reconciliation::source_constructor_misuse_remains_source_owned_fixture_passes())
    {
        let checks: &[(&str, fn() -> bool)] = &[
            (
                "verification_failure_preserves_repair_targets",
                crate::agent::state::verification_failure_preserves_repair_targets_fixture_passes,
            ),
            (
                "source_owned_verification_failure_preserves_recent_source_edit_target",
                crate::agent::state::source_owned_verification_failure_preserves_recent_source_edit_target_fixture_passes,
            ),
            (
                "verification_timeout_preserves_recent_source_repair_target",
                crate::agent::state::verification_timeout_preserves_recent_source_repair_target_fixture_passes,
            ),
            (
                "out_of_order_history_items_use_sequence_authority_for_repair",
                crate::agent::state::out_of_order_history_items_use_sequence_authority_for_repair_fixture_passes,
            ),
            (
                "verification_failure_diagnostic_labels_do_not_become_repair_targets",
                crate::agent::state::verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes,
            ),
            (
                "verification_failure_diagnostic_paths_are_not_requested_work_targets",
                crate::agent::state::verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes,
            ),
            (
                "continuation_context_symbols_are_not_requested_work_targets",
                crate::agent::state::continuation_context_symbols_are_not_requested_work_targets_fixture_passes,
            ),
            (
                "verification_repair_continuation_projects_repair_state",
                crate::agent::state::verification_repair_continuation_projects_repair_state_fixture_passes,
            ),
            (
                "public_command_contract_continuation_projects_compact_source_repair",
                crate::agent::state::public_command_contract_continuation_projects_compact_source_repair_fixture_passes,
            ),
            (
                "verification_repair_continuation_generated_test_parse_target",
                crate::agent::state::verification_repair_continuation_generated_test_parse_target_fixture_passes,
            ),
            (
                "verification_failure_ignores_runtime_loader_frame",
                crate::agent::state::verification_failure_ignores_runtime_loader_frame_fixture_passes,
            ),
            (
                "verification_repair_targets_from_state_ignore_diagnostic_scalars",
                crate::agent::state::verification_repair_targets_from_state_ignore_diagnostic_scalars_fixture_passes,
            ),
            (
                "message_user_protected_reference_filters_verification_targets",
                crate::agent::state::message_user_protected_reference_filters_verification_targets_fixture_passes,
            ),
            (
                "public_output_stream_source_repair_active_work_uses_source_target",
                crate::agent::state::public_output_stream_source_repair_active_work_uses_source_target_fixture_passes,
            ),
            (
                "source_owned_repair_active_work_excludes_generated_test_evidence",
                crate::agent::state::source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes,
            ),
            (
                "source_owned_requirement_refs_align_active_work_with_repair_lane",
                crate::agent::state::source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes,
            ),
            (
                "contract_visible_public_exception_active_work_targets_source",
                crate::agent::state::contract_visible_public_exception_active_work_targets_source_fixture_passes,
            ),
            (
                "generated_test_validity_active_work_outranks_source_sibling",
                crate::agent::state::generated_test_validity_active_work_outranks_source_sibling_fixture_passes,
            ),
            (
                "generated_test_parse_defect_active_work_matches_repair_lane",
                crate::agent::state::generated_test_parse_defect_active_work_matches_repair_lane_fixture_passes,
            ),
            (
                "generated_test_api_misuse_active_work_targets_test",
                crate::agent::state::generated_test_api_misuse_active_work_targets_test_fixture_passes,
            ),
            (
                "generated_test_module_attribute_api_misuse_active_work_targets_test",
                crate::agent::state::generated_test_module_attribute_api_misuse_active_work_targets_test_fixture_passes,
            ),
            (
                "generated_test_exception_type_overreach_active_work_targets_test",
                crate::agent::state::generated_test_exception_type_overreach_active_work_targets_test_fixture_passes,
            ),
            (
                "generated_test_local_binding_contradiction_active_work",
                crate::agent::state::generated_test_local_binding_contradiction_active_work_fixture_passes,
            ),
            (
                "post_repair_generated_test_public_output_overreach_enters_test_repair",
                crate::agent::state::post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes,
            ),
            (
                "public_command_contract_failure_projects_compact_source_repair",
                crate::agent::repair_lane::public_command_contract_failure_projects_compact_source_repair_fixture_passes,
            ),
            (
                "generated_test_subprocess_encoding_missing_projects_test_repair",
                crate::agent::repair_lane::generated_test_subprocess_encoding_missing_projects_test_repair_fixture_passes,
            ),
            (
                "generated_test_reflection_api_misuse_projects_test_repair",
                crate::agent::repair_lane::generated_test_reflection_api_misuse_projects_test_repair_fixture_passes,
            ),
            (
                "generated_test_module_attribute_api_misuse_projects_test_repair",
                crate::agent::repair_lane::generated_test_module_attribute_api_misuse_projects_test_repair_fixture_passes,
            ),
            (
                "generated_test_exception_type_overreach_projects_test_repair",
                crate::agent::repair_lane::generated_test_exception_type_overreach_projects_test_repair_fixture_passes,
            ),
            (
                "mixed_generated_test_validity_and_public_behavior_preserves_source_test_mismatch",
                crate::agent::contract_reconciliation::mixed_generated_test_validity_and_public_behavior_preserves_source_test_mismatch_fixture_passes,
            ),
            (
                "generated_test_api_misuse_without_source_public_api_is_test_owned",
                crate::agent::contract_reconciliation::generated_test_api_misuse_without_source_public_api_is_test_owned_fixture_passes,
            ),
            (
                "generated_test_exception_type_overreach_is_test_owned",
                crate::agent::contract_reconciliation::generated_test_exception_type_overreach_is_test_owned_fixture_passes,
            ),
        ];
        let failed_fixtures = checks
            .iter()
            .filter_map(|(name, check)| (!check()).then_some(*name))
            .collect::<Vec<_>>();
        diagnostics.push(
            format!(
                "verification failure projection can still erase source/test repair targets, split source-owned repair target authority, or fail-closed before dispatch; failed fixtures: {}",
                failed_fixtures.join(", ")
            ),
        );
    }

    if gate.gate_id == "preflight.state_reducer.docs_route_contract_authority" {
        if !crate::agent::state::docs_route_contract_promotes_docs_repair_fixture_passes() {
            diagnostics.push(
                "docs-only route contract can still degrade to generic requested-work authoring"
                    .to_string(),
            );
        }
        if !crate::agent::state::docs_route_single_deliverable_contract_promotes_docs_repair_fixture_passes()
        {
            diagnostics.push(
                "singleton docs-only route contract can still degrade to generic requested-work authoring"
                    .to_string(),
            );
        }
        let same_document_checks = [
            (
                "same_document_reference_update_remains_authoring_target",
                crate::agent::state::same_document_reference_update_remains_authoring_target_fixture_passes(),
            ),
            (
                "same_document_update_uses_prior_authored_doc_not_contract_ref",
                crate::agent::state::same_document_update_uses_prior_authored_doc_not_contract_ref_fixture_passes(),
            ),
            (
                "same_document_update_stays_pending_after_prior_doc_satisfied",
                crate::agent::state::same_document_update_stays_pending_after_prior_doc_satisfied_fixture_passes(),
            ),
        ];
        let failed_same_document_checks = same_document_checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed_same_document_checks.is_empty() {
            diagnostics.push(format!(
                "same-document docs-only updates can still fall out of DocsRepair or promote protected references into active deliverables; failed fixtures: {}",
                failed_same_document_checks.join(", ")
            ));
        }
        if !crate::agent::state::docs_route_contract_does_not_require_unmentioned_web_areas_fixture_passes() {
            diagnostics.push("docs-only route contract can still inherit fixed web/data/example area coverage from representative fixtures".to_string());
        }
        if !crate::agent::state::docs_route_flat_test_artifact_satisfies_required_area_fixture_passes()
        {
            diagnostics.push(
                "docs-only route contract can still ignore flat root-level test artifact evidence for required tests area coverage".to_string(),
            );
        }
        if !crate::agent::state::docs_route_localized_topic_completion_fixture_passes() {
            diagnostics.push(
                "docs-only route localized topic completion can still leave stale pending state"
                    .to_string(),
            );
        }
        if !crate::agent::state::docs_route_closeout_continuation_preserves_docs_authority_fixture_passes()
        {
            diagnostics.push(
                "docs-only closeout continuation can still degrade a missing docs artifact into generic requested-work authoring".to_string(),
            );
        }
        if !crate::agent::loop_impl::provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
        {
            diagnostics.push(
                "docs-only exact write recovery can still drop prior supporting-context evidence when the effective tool surface narrows".to_string(),
            );
        }
        if !crate::agent::loop_impl::docs_route_final_message_recovery_requires_content_grounding_fixture_passes()
        {
            diagnostics.push(
                "docs-only final-message recovery can still narrow to exact write before content-bearing repository evidence exists".to_string(),
            );
        }
        if !crate::agent::loop_impl::docs_route_semantic_no_progress_guard_fixture_passes()
            || !crate::agent::tool_orchestrator::docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes()
            || !crate::agent::loop_impl::docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_survives_partial_write_fixture_passes()
            || !crate::agent::loop_impl::docs_route_rejects_completed_deliverable_regression_fixture_passes()
        {
            diagnostics.push(
                "docs-only route contract can still enter unbounded read-only churn or accept contract-regressing docs progress".to_string(),
            );
        }
        if !crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes() {
            diagnostics.push("docs-only route prompt projection can still miss the write-ready docs boundary".to_string());
        }
        if !crate::harness::manual_st::satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes()
        {
            diagnostics.push(
                "docs-only route closeout can still use natural-language pending summary as authority instead of typed route_contract_satisfied state".to_string(),
            );
        }
    }

    if gate.gate_id == "preflight.docs_spec.semantic_reconciliation_before_handoff"
        && (!crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_fixture_passes()
            || !crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_tool_fixture_passes()
            || !crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_feedback_projection_fixture_passes()
            || !crate::agent::tool_orchestrator::docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes()
            || !crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes())
    {
        diagnostics.push(
            "docs/spec authoring can still accept artifact progress that misses required latest-request claims or includes prohibited contradictory claims before handoff".to_string(),
        );
    }

    if gate.gate_id == "preflight.verification.public_command_contract_coverage"
        && (!crate::agent::public_command_contract::public_command_contract_fixture_passes()
            || !crate::agent::public_command_contract::public_command_contract_apply_patch_uses_post_patch_content_fixture_passes()
            || !crate::agent::repair_lane::public_command_contract_failure_projects_compact_source_repair_fixture_passes()
            || !crate::harness::manual_st::public_command_contract_closeout_prompt_compacts_failure_evidence_fixture_passes()
            || !crate::harness::manual_st::public_command_contract_route_evidence_fixture_passes()
            || !crate::harness::manual_st::route_verification_process_environment_fixture_passes())
    {
        diagnostics.push(
            "prompt/spec-visible public command contracts can still be satisfied by generated tests that omit argv/exit/stdout evidence, generated child subprocess tests can still omit bounded timeout authority, incremental patch validation can still ignore existing generated-test coverage, route evidence cannot represent expected nonzero command outcomes as passing contract checks, route-owned public command failures can still expand into raw traceback repair prompts instead of compact source repair, or route-owned verification can still diverge from shell tool UTF-8 process environment / output decoding authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.verification.command_correction_satisfies_obligation"
        && (!crate::agent::state::corrected_verification_command_consumes_original_obligation_fixture_passes()
            || !crate::agent::loop_impl::singleton_verification_command_arguments_are_runtime_owned_fixture_passes()
            || !crate::protocol::verification_only_authority_narrows_to_exact_shell_fixture_passes())
    {
        diagnostics.push(
            "corrected verification command executions can still pass without consuming the original required verification obligation, or prompt/request action authority can still require the rejected literal command instead of the corrected executable form".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.synthetic_feedback_not_verification_authority"
        && (!crate::agent::tool_orchestrator::synthetic_corrective_shell_feedback_is_not_verification_run_fixture_passes()
            || !crate::agent::state::synthetic_tool_feedback_preserves_real_verification_cluster_fixture_passes())
    {
        diagnostics.push(
            "synthetic corrective/tool-policy feedback can still overwrite real verification evidence".to_string(),
        );
    }

    PreflightGateReport {
        gate_id: gate.gate_id.clone(),
        fixture_id: Some(fixture.fixture_id.clone()),
        layer: gate.layer,
        family: Some(gate.family),
        status: if diagnostics.is_empty() {
            PreflightResultStatus::Pass
        } else {
            PreflightResultStatus::Fail
        },
        diagnostics,
        evidence_refs: fixture.required_refs.clone(),
    }
}

fn desktop_transcript_primary_reading_fixture_passes() -> bool {
    #[cfg(feature = "tauri-desktop")]
    {
        crate::desktop::query::completed_desktop_transcript_primary_reading_fixture_passes()
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        true
    }
}

fn desktop_turn_item_projection_sequence_fixture_passes() -> bool {
    #[cfg(feature = "tauri-desktop")]
    {
        crate::desktop::query::desktop_turn_item_projection_uses_turn_local_sequence_fixture_passes(
        )
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        true
    }
}

fn desktop_file_change_projection_sequence_fixture_passes() -> bool {
    #[cfg(feature = "tauri-desktop")]
    {
        crate::desktop::artifact_projection::desktop_file_change_projection_uses_turn_local_sequence_fixture_passes()
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        true
    }
}

fn protocol_persistence_unit_of_work_fixture_passes() -> bool {
    use crate::protocol::{ProtocolEventStore, RuntimeEventMsg, TurnId};
    use crate::session::{
        AssistantMessageMeta, FinishReason, MessageMetadata, MessageRole, NewMessage, NewSession,
        ProjectId, ProjectRepository, RunEvent, SessionRepository, SessionStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths};

    let unique = format!(
        "moyai-preflight-uow-{}-{}",
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
    let result = std::thread::spawn(move || -> Result<bool, RuntimeError> {
        let store = SqliteStore::open(&worker_paths)
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        store
            .migrate()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        let project_repo = store.project_repo();
        let session_repo = store.session_repo();
        let protocol_store = store.protocol_event_store();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        runtime.block_on(async {
            let project_id = ProjectId::new();
            let workspace_root = Utf8Path::new("C:/workspace/protocol-uow");
            project_repo
                .upsert_project(project_id, workspace_root, "Protocol UoW", "none")
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let session = session_repo
                .create_session(NewSession {
                    project_id,
                    title: "protocol unit of work".to_string(),
                    cwd: workspace_root.to_path_buf(),
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                })
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
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
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let terminal_event = RunEvent::SessionCompleted {
                session_id: session.id,
                finish_reason: Some(FinishReason::Stop),
            };
            session_repo
                .update_message_metadata_and_status_with_protocol_event(
                    session.id,
                    assistant.id,
                    &MessageMetadata::Assistant(AssistantMessageMeta {
                        model: "model".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: Some(FinishReason::Stop),
                        token_usage: None,
                        summary: false,
                    }),
                    SessionStatus::Completed,
                    &terminal_event,
                    turn_id,
                    Some(1),
                )
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let stored_session = session_repo
                .get_session(session.id)
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let transcript = session_repo
                .transcript(session.id)
                .await
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let runtime_events = protocol_store
                .list_runtime_events(session.id, turn_id)
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            let terminal_metadata = transcript
                .messages
                .iter()
                .find(|message| message.record.id == assistant.id)
                .map(|message| message.record.metadata.clone());
            Ok(stored_session.status == SessionStatus::Completed
                && matches!(
                    terminal_metadata,
                    Some(MessageMetadata::Assistant(AssistantMessageMeta {
                        finish_reason: Some(FinishReason::Stop),
                        ..
                    }))
                )
                && runtime_events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::AssistantStarted { .. }))
                && runtime_events
                    .iter()
                    .any(|event| matches!(event.msg, RuntimeEventMsg::TurnCompleted { .. })))
        })
    })
    .join()
    .unwrap_or_else(|_| {
        Err(RuntimeError::Message(
            "preflight storage worker panicked".to_string(),
        ))
    });
    let _ = fs::remove_dir_all(data_dir.as_std_path());
    result.unwrap_or(false)
}

fn protocol_item_lifecycle_fixture_passes() -> bool {
    use crate::config::{AccessMode, ShellFamily};
    use crate::protocol::{
        ActiveWorkContractProjection, HistoryItemPayload, ModelCapabilities, OutputContract,
        ProtocolEventStore, ProtocolRecordingSink, RuntimeEventMsg, SandboxProfile,
        SqliteProtocolEventStore, ToolChoice, TurnContext, TurnId, UserInputItem, UserTurn,
    };
    use crate::runtime::RunEventSink;
    use crate::session::{ProcessPhase, RunEvent, SessionId, TaskRoute, ToolCallId};
    use crate::tool::ToolName;
    use camino::Utf8PathBuf;
    use std::sync::{Arc, Mutex};

    let connection = match rusqlite::Connection::open_in_memory() {
        Ok(value) => value,
        Err(_) => return false,
    };
    if crate::storage::migration::run(&connection).is_err() {
        return false;
    }
    let store = SqliteProtocolEventStore::new(Arc::new(Mutex::new(connection)));
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    #[derive(Default)]
    struct NullSink;
    impl RunEventSink for NullSink {
        fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
            Ok(())
        }
    }
    let mut inner = NullSink;
    let mut sink = ProtocolRecordingSink::new(store.clone(), Some(session_id), turn_id, &mut inner);
    let call_id = ToolCallId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_work_kind: Some("fixture".to_string()),
        summary: "create right.py".to_string(),
        active_targets: vec![Utf8PathBuf::from("right.py")],
        operation_intents: Vec::new(),
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::Write],
        forbidden_tools: Vec::new(),
        projection_id: crate::protocol::ProjectionId::new(),
    };
    let user_turn = UserTurn {
        turn_id,
        items: vec![UserInputItem::Text {
            text: "create right.py".to_string(),
        }],
        prompt_dispatch: None,
        editor_context: None,
        context: TurnContext {
            session_id,
            cwd: Utf8PathBuf::from("C:/workspace"),
            workspace_root: Utf8PathBuf::from("C:/workspace"),
            provider: "openai_compat".to_string(),
            model: "local-model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            access_mode: AccessMode::AutoReview,
            sandbox: SandboxProfile::WorkspaceWrite,
            shell_family: ShellFamily::PowerShell,
            model_capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
                parallel_tool_calls: false,
                context_window: 8192,
                max_output_tokens: 1024,
            },
            route: TaskRoute::Code,
            process_phase: ProcessPhase::Author,
            active_contract,
            allowed_tools: vec![ToolName::Write],
            tool_choice: ToolChoice::Auto,
            images: Vec::new(),
            output_contract: OutputContract {
                final_answer_required: true,
                structured_schema_name: None,
                history_markdown_projection: true,
            },
            continuation: None,
            turn_decision_projection: None,
        },
    };
    if sink
        .emit(RunEvent::UserTurnStored {
            session_id,
            message_id: crate::session::MessageId::new(),
            turn: Box::new(user_turn),
        })
        .is_err()
    {
        return false;
    }
    let pending_metadata = serde_json::json!({
        "tool_route": {
            "original_arguments": {"path": "wrong.py", "content": "old"},
            "effective_arguments": {"path": "right.py", "content": "new"},
            "adjusted_arguments": {"path": "right.py", "content": "new"},
            "allowed_tools": ["write"],
            "permission_decision": "pending",
            "sandbox_decision": {"profile": "workspace_write", "network_allowed": false, "escalated": false}
        }
    });
    if sink
        .emit(RunEvent::ToolCallPending {
            tool_call_id: call_id,
            tool: ToolName::Write,
            title: "write".to_string(),
            metadata: pending_metadata,
        })
        .is_err()
    {
        return false;
    }
    if sink
        .emit(RunEvent::ToolCallCompleted {
            tool_call_id: call_id,
            tool: ToolName::Write,
            title: "write complete".to_string(),
            summary: "ok".to_string(),
            metadata: serde_json::json!({
                "success": true,
                "progress_effect": "made_progress",
                "tool_feedback_envelope": {"result_hash": "hash"}
            }),
        })
        .is_err()
    {
        return false;
    }
    if sink
        .emit(RunEvent::SessionCompleted {
            session_id,
            finish_reason: None,
        })
        .is_err()
    {
        return false;
    }
    let events = match store.list_runtime_events(session_id, turn_id) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let turn_started = events
        .iter()
        .position(|event| matches!(event.msg, RuntimeEventMsg::TurnStarted { .. }));
    let terminal = events.iter().position(|event| {
        matches!(
            event.msg,
            RuntimeEventMsg::TurnCompleted { .. }
                | RuntimeEventMsg::TurnFailed { .. }
                | RuntimeEventMsg::TurnInterrupted { .. }
                | RuntimeEventMsg::TurnAwaitingUser { .. }
        )
    });
    let Some(turn_started) = turn_started else {
        return false;
    };
    let Some(terminal) = terminal else {
        return false;
    };
    if turn_started >= terminal {
        return false;
    }
    let items = match store.list_history_items_for_session(session_id) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let has_effective_args = items.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::ToolCall {
                arguments,
                model_arguments,
                effective_arguments,
                adjusted_arguments: Some(_),
                ..
            } if arguments.get("path").and_then(serde_json::Value::as_str) == Some("right.py")
                && model_arguments.get("path").and_then(serde_json::Value::as_str) == Some("wrong.py")
                && effective_arguments.get("path").and_then(serde_json::Value::as_str) == Some("right.py")
        )
    });
    let has_typed_success = items.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::ToolOutput {
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                result_hash: Some(hash),
                ..
            } if hash == "hash"
        )
    });
    let turn_items = match store.list_turn_items_for_session(session_id) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let has_user_item = turn_items.iter().any(|item| {
        matches!(
            item.payload,
            crate::protocol::TurnItemPayload::UserMessage { .. }
        )
    });
    let has_terminal_item = turn_items.iter().any(|item| {
        matches!(
            item.payload,
            crate::protocol::TurnItemPayload::Terminal {
                status: crate::protocol::TurnTerminalStatus::Completed,
                ..
            }
        )
    });
    has_effective_args && has_typed_success && has_user_item && has_terminal_item
}

fn state_reducer_runtime_feedback_fixture_passes() -> bool {
    crate::agent::state::runtime_feedback_summary_preserves_completion_authority(
        "The previous response did not use any tools while typed work remains: \
         active_work=author test_component.py. Runtime requires a call through one of the \
         currently allowed tools or a closeout-ready completion state before session completion.",
    )
}

fn plan_progress_projection_fixture_passes() -> bool {
    crate::agent::state::requested_work_missing_todo_graph_stays_authoring_authority()
        && crate::tool::todo_write::progress_projection_payload_drops_authority_fields()
        && crate::agent::prompt_assets::planning_prompt_keeps_todowrite_side_channel_fixture_passes()
        && crate::agent::loop_impl::progress_projection_loop_terminal_guard_fixture_passes()
        && crate::agent::loop_impl::open_authoring_operation_intent_classifies_non_content_tools_fixture_passes()
        && crate::agent::loop_impl::open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
}

fn prompt_replay_stale_write_failed_fixtures() -> Vec<String> {
    let mut failed = Vec::new();
    if !crate::agent::prompt::stale_write_tool_call_replay_is_summary_only(
        r#"{"path":"component.py","content":"previous source content"}"#,
        "test_component.py",
    ) {
        failed.push("stale_write_tool_call_replay_is_summary_only".to_string());
    }
    if !crate::agent::prompt::stale_write_tool_call_replay_omits_payload(
        r#"{"path":"component.py","content":"def render(): pass"}"#,
        "test_component.py",
        "def render",
    ) {
        failed.push("stale_write_tool_call_replay_omits_payload".to_string());
    }
    if !crate::agent::prompt::stale_write_prelude_replay_omits_text(
        "test_component.py",
        "component.py",
    ) {
        failed.push("stale_write_prelude_replay_omits_text".to_string());
    }
    if !crate::agent::prompt::stale_todo_progress_replay_omits_prior_plan(
        "test_component.py",
        "Create `component.py` and `test_component.py`.",
    ) {
        failed.push("stale_todo_progress_replay_omits_prior_plan".to_string());
    }
    if !prompt_replay_internal_control_items_are_not_provider_visible() {
        failed.push("prompt_replay_internal_control_items_are_not_provider_visible".to_string());
    }
    if !write_schema_stays_provider_owned() {
        failed.push("write_schema_stays_provider_owned".to_string());
    }
    if !crate::agent::prompt::exact_write_target_contract_projects_content_authority(
        "test_component.py",
    ) {
        failed.push("exact_write_target_contract_projects_content_authority".to_string());
    }
    if !crate::agent::loop_impl::required_write_target_mismatch_feedback_projects_test_content_authority() {
        failed.push("required_write_target_mismatch_feedback_projects_test_content_authority".to_string());
    }
    if !crate::agent::loop_impl::concrete_write_required_action_narrows_broad_surface_fixture_passes(
    ) {
        failed.push("concrete_write_required_action_narrows_broad_surface".to_string());
    }
    if !crate::agent::loop_impl::exact_write_route_accepts_unittest_main_test_content() {
        failed.push("exact_write_route_accepts_unittest_main_test_content".to_string());
    }
    if !crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden() {
        failed.push("test_target_content_shape_projection_is_positive_and_forbidden".to_string());
    }
    if !crate::agent::loop_impl::content_shape_mismatch_feedback_carries_positive_test_contract() {
        failed.push("content_shape_mismatch_feedback_carries_positive_test_contract".to_string());
    }
    if !crate::agent::tool_result_classification::required_write_content_shape_mismatch_is_nonprogress() {
        failed.push("required_write_content_shape_mismatch_is_nonprogress".to_string());
    }
    if !crate::agent::loop_impl::test_target_content_shape_write_lifecycle_enforced_fixture_passes()
    {
        failed.push("test_target_content_shape_write_lifecycle_enforced".to_string());
    }
    if !crate::agent::loop_impl::test_target_content_shape_rejects_string_literal_wrapped_tests_fixture_passes()
    {
        failed.push("test_target_content_shape_rejects_string_literal_wrapped_tests".to_string());
    }
    if !crate::agent::loop_impl::source_content_shape_rejects_escaped_whole_file_fixture_passes() {
        failed.push("source_content_shape_rejects_escaped_whole_file".to_string());
    }
    if !crate::agent::loop_impl::source_content_shape_normalizes_escaped_repair_candidate_fixture_passes()
    {
        failed.push("source_content_shape_normalizes_escaped_repair_candidate".to_string());
    }
    if !crate::agent::loop_impl::source_content_shape_rejects_test_module_payload_fixture_passes() {
        failed.push("source_content_shape_rejects_test_module_payload".to_string());
    }
    if !crate::agent::loop_impl::source_content_shape_rejects_markdown_payload_fixture_passes() {
        failed.push("source_content_shape_rejects_markdown_payload".to_string());
    }
    if !crate::agent::loop_impl::source_content_shape_rejects_raw_prose_line_fixture_passes() {
        failed.push("source_content_shape_rejects_raw_prose_line".to_string());
    }
    if !crate::agent::loop_impl::corrective_content_shape_no_progress_terminal_guard_fixture_passes(
    ) {
        failed.push("corrective_content_shape_no_progress_terminal_guard".to_string());
    }
    if !crate::agent::loop_impl::content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes() {
        failed.push(
            "content_shape_failed_edit_projects_latest_recovery_into_control_envelope".to_string(),
        );
    }
    if !crate::agent::loop_impl::text_artifact_content_shape_rejects_serialized_markdown_fixture_passes()
    {
        failed.push("text_artifact_content_shape_rejects_serialized_markdown".to_string());
    }
    if !crate::agent::prompt::text_artifact_content_shape_repair_projection_carries_positive_contract(
    ) {
        failed.push(
            "text_artifact_content_shape_repair_projection_carries_positive_contract".to_string(),
        );
    }
    if !crate::agent::prompt::python_source_content_shape_repair_projection_carries_positive_contract(
    ) {
        failed.push(
            "python_source_content_shape_repair_projection_carries_positive_contract".to_string(),
        );
    }
    if !crate::agent::loop_impl::final_dispatch_source_schema_projection_fixture_passes() {
        failed.push("final_dispatch_source_schema_projection".to_string());
    }
    if !crate::agent::loop_impl::content_shape_mismatch_canonicalizes_workspace_absolute_target_fixture_passes()
    {
        failed.push("content_shape_mismatch_canonicalizes_workspace_absolute_target".to_string());
    }
    if !crate::agent::loop_impl::test_target_content_shape_apply_patch_post_content_enforced_fixture_passes() {
        failed.push("test_target_content_shape_apply_patch_post_content_enforced".to_string());
    }
    if !crate::tool::apply_patch::destructive_noop_patch_is_rejected_fixture_passes() {
        failed.push("destructive_noop_patch_is_rejected".to_string());
    }
    if !crate::tool::apply_patch::add_file_unprefixed_content_line_feedback_names_line_fixture_passes()
    {
        failed.push("add_file_unprefixed_content_line_feedback_names_line".to_string());
    }
    if !crate::agent::prompt::content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload(
    ) {
        failed.push(
            "content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload".to_string(),
        );
    }
    if !crate::agent::prompt::exact_write_repair_omits_consumed_supporting_context_replay() {
        failed.push("exact_write_repair_omits_consumed_supporting_context_replay".to_string());
    }
    if !crate::agent::prompt::stale_inactive_authoring_replay_uses_live_builder() {
        failed.push("stale_inactive_authoring_replay_uses_live_builder".to_string());
    }
    if !crate::agent::prompt::exact_authoring_write_required_preserves_source_progress_projection()
    {
        failed.push(
            "exact_authoring_write_required_preserves_source_progress_projection".to_string(),
        );
    }
    failed
}

fn prompt_replay_internal_control_items_are_not_provider_visible() -> bool {
    use crate::protocol::{ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, TurnId};
    use crate::session::{
        ProjectId, SessionId, SessionRecord, SessionStateSnapshot, SessionStatus,
        transcript_from_history_items,
    };

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "preflight prompt projection".to_string(),
        status: SessionStatus::Running,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut stale_state = SessionStateSnapshot::default();
    stale_state
        .active_targets
        .push(camino::Utf8PathBuf::from("source_module.py"));

    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "create test_component.py".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::SessionState { state: stale_state },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "runtime/session failure is not assistant content".to_string(),
            },
        },
    ];
    let transcript = transcript_from_history_items(&session, &items);
    let provider_visible_text =
        crate::session::transcript::flatten_text_parts(&transcript).join("\n");
    provider_visible_text.contains("test_component.py")
        && !provider_visible_text.contains("source_module.py")
        && !provider_visible_text.contains("runtime/session failure")
        && transcript
            .messages
            .iter()
            .all(|message| message.record.sequence_no != 2 && message.record.sequence_no != 3)
}

fn provider_replay_call_output_symmetry_fixture_passes() -> bool {
    use crate::llm::ModelMessage;
    use crate::protocol::{
        ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, ToolLifecycleStatus,
        ToolProgressEffect, TurnId,
    };
    use crate::session::{ProjectId, SessionId, SessionRecord, SessionStatus, ToolCallId};
    use crate::tool::ToolName;

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let orphan_call_id = ToolCallId::new();
    let interrupted_call_id = ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "provider replay preflight".to_string(),
        status: SessionStatus::Running,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "inspect workspace".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::List,
                arguments: serde_json::json!({"path": "."}),
                model_arguments: serde_json::json!({"path": "."}),
                effective_arguments: serde_json::json!({"path": "."}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::List],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "listed".to_string(),
                output_text: "component.py".to_string(),
                metadata: serde_json::json!({"success": true}),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("hash".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: orphan_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "orphan".to_string(),
                output_text: "orphan output must not be assistant text".to_string(),
                metadata: serde_json::json!({}),
                success: Some(true),
                progress_effect: ToolProgressEffect::Unknown,
                blocked_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolCall {
                call_id: interrupted_call_id,
                tool: ToolName::Read,
                arguments: serde_json::json!({"path": "unfinished.py"}),
                model_arguments: serde_json::json!({"path": "unfinished.py"}),
                effective_arguments: serde_json::json!({"path": "unfinished.py"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Read],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 6,
            created_at_ms: 6,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "runtime error must not replay".to_string(),
            },
        },
    ];
    let replay = crate::agent::prompt::build_provider_replay_messages_from_history_items(
        &session, &items, 32,
    );
    let serialized = serde_json::to_string(&replay).unwrap_or_default();
    replay.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|tool_call| tool_call.call_id == call_id.to_string())
        )
    }) && replay.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id: replayed, result, .. }
                if replayed == &call_id.to_string() && result.contains("component.py")
        )
    }) && replay.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id: replayed, result, .. }
                if replayed == &interrupted_call_id.to_string() && result == "aborted"
        )
    }) && !serialized.contains("orphan output must not be assistant text")
        && !serialized.contains("runtime error must not replay")
}

fn write_schema_stays_provider_owned() -> bool {
    let mut tools = vec![crate::llm::ToolSchema {
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
    }];
    crate::agent::loop_impl::preserve_provider_tool_surface_for_dispatch(&mut tools);
    let Some(tool) = tools.first() else {
        return false;
    };
    !tool.strict
        && tool.input_schema.pointer("/properties/path").is_some()
        && tool
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| {
                values.iter().any(|value| value.as_str() == Some("path"))
                    && values.iter().any(|value| value.as_str() == Some("content"))
            })
}

fn is_generic_primary_key(gate_id: &str) -> bool {
    let lower = gate_id.to_ascii_lowercase();
    !lower.contains("case1")
        && !lower.contains("case2")
        && !lower.contains("case3")
        && !lower.contains("case4")
        && !lower.contains("case5")
        && !lower.contains("case6")
        && !lower.contains("case7")
        && !lower.contains("manual_st.")
}

pub fn failure_registry_preflight_suite() -> Vec<PreflightGate> {
    vec![
        PreflightGate {
            gate_id: "preflight.protocol.history_item_lifecycle_authority".to_string(),
            purpose: "canonical runtime events, HistoryItem, and TurnItem streams expose explicit Codex-style turn boundaries and remain the harness authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ProtocolItemLifecycle,
            fixture_id: "fixture.protocol.history_item_lifecycle_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.protocol.persistence_unit_of_work_authority".to_string(),
            purpose: "compatibility transcript rows, session status, and canonical protocol projection are persisted by the same runtime unit-of-work rather than divergent display/harness writes".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ProtocolItemLifecycle,
            fixture_id: "fixture.protocol.persistence_unit_of_work_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.item_lifecycle.provider_replay_call_output_symmetry".to_string(),
            purpose: "provider replay is built directly from canonical HistoryItem call/output pairs; orphan outputs and runtime errors cannot become assistant text".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ProtocolItemLifecycle,
            fixture_id: "fixture.item_lifecycle.provider_replay_call_output_symmetry".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.llm_transport.stream_retry_before_first_event".to_string(),
            purpose: "retry provider SSE transport/body decode failures only before any model event has been emitted, avoiding duplicate partial output".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::LlmTransportAuthority,
            fixture_id: "fixture.llm_transport.stream_retry_before_first_event".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.llm_transport.streaming_timeout_boundary".to_string(),
            purpose: "streaming requests use request timeout for response headers and stream idle timeout for body progress, not one total-body timeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::LlmTransportAuthority,
            fixture_id: "fixture.llm_transport.streaming_timeout_boundary".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.control_envelope.dispatch_projection_authority".to_string(),
            purpose: "provider dispatch uses a typed TurnControlEnvelope and fails closed on missing projection".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ControlEnvelopeProjection,
            fixture_id: "fixture.control_envelope.dispatch_projection_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.state_reducer.runtime_feedback_classification_authority"
                .to_string(),
            purpose: "recoverable runtime feedback remains tool/completion feedback and cannot become verification repair authority without typed verification evidence".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id: "fixture.state_reducer.runtime_feedback_classification_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.state_reducer.requested_work_completion_promotes_verification"
                .to_string(),
            purpose: "completed requested-work authoring promotes exact verification shell authority before closeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id:
                "fixture.state_reducer.requested_work_completion_promotes_verification"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.state_reducer.post_repair_edit_promotes_verification_rerun"
                .to_string(),
            purpose: "successful content-changing repair FileChangeEvidence satisfies the current repair target, clears evidence-only generated-test edit obligations for source-owned repair, promotes the next dispatch to exact verification rerun, and treats single-command verification as a runtime-owned required action instead of provider-compliance churn".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id:
                "fixture.state_reducer.post_repair_edit_promotes_verification_rerun"
                    .to_string(),
        },
        PreflightGate {
            gate_id:
                "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
                    .to_string(),
            purpose: "failed verification outputs preserve prior obligation targets and project typed source-owned repair authority as a single provider-visible edit target before the next provider dispatch".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id:
                "fixture.state_reducer.verification_failure_preserves_repair_target_authority"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.state_reducer.docs_route_contract_authority".to_string(),
            purpose: "docs-only long-context tasks and closeout continuation turns materialize as TaskRoute::Docs with DocsRouteState and DocsRepair, then bound read-only churn through call-id-scoped corrective tool output and a write-ready docs lifecycle boundary before generic requested-work filename authoring can own the turn".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id: "fixture.state_reducer.docs_route_contract_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.docs_spec.semantic_reconciliation_before_handoff".to_string(),
            purpose: "documentation/spec writes reconcile required and prohibited claims from latest request authority before file-change progress or clean handoff can be accepted".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.docs_spec.semantic_reconciliation_before_handoff".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.verification.public_command_contract_coverage".to_string(),
            purpose: "prompt/spec-visible public command examples become typed coverage obligations in generated tests and route evidence; write validation uses submitted content, patch validation uses projected post-patch content, parent UTF-8 decoding requires child output encoding authority, CompletedProcess stdout/stderr assertions require captured streams, and an internal test pass cannot hide missing argv/exit/stdout behavior".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            fixture_id: "fixture.verification.public_command_contract_coverage".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.verification.command_correction_satisfies_obligation".to_string(),
            purpose: "corrected/effective verification commands keep typed identity with the original required command, so a passing corrected execution consumes the original obligation without duplicating command strings".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            fixture_id:
                "fixture.verification.command_correction_satisfies_obligation".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.plan_progress_projection.todo_absence_does_not_gate_authoring"
                .to_string(),
            purpose: "missing progress projection cannot block a known requested-work write authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PlanProgressProjectionAuthority,
            fixture_id:
                "fixture.plan_progress_projection.todo_absence_does_not_gate_authoring"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.stale_write_arguments_summary_projection"
                .to_string(),
            purpose: "stale write arguments, assistant payload text, and prior todo progress output for completed previous targets are omitted from provider-visible replay instead of competing with current action authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.stale_write_arguments_summary_projection"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.active_user_hook_non_droppable".to_string(),
            purpose: "provider replay preserves the latest real user or hook prompt as the current input even when a trailing compaction item summarizes older context".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.active_user_hook_non_droppable".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.tool_pair_symmetry".to_string(),
            purpose: "provider replay preserves call-id-scoped assistant tool-call/tool-output symmetry, uses model_arguments when compatibility arguments are absent, and sanitizes current malformed edit arguments without dropping failed ToolOutput evidence".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.tool_pair_symmetry".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.compaction_orphan_assistant_repaired".to_string(),
            purpose: "provider replay after compaction restores the matching user query for an assistant/tool item that would otherwise become orphaned before the latest user query".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.compaction_orphan_assistant_repaired".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.stale_inactive_authoring_pair_omitted".to_string(),
            purpose: "provider replay omits stale inactive authoring tool-call/output pairs as executable history instead of exposing fake sentinel arguments".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.stale_inactive_authoring_pair_omitted"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.prompt_replay.progress_projection_pair_omitted".to_string(),
            purpose: "provider replay omits stale progress-projection tool-call/output pairs as executable history while preserving current call-id-scoped no-progress feedback for active targets".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id: "fixture.prompt_replay.progress_projection_pair_omitted".to_string(),
        },
        PreflightGate {
            gate_id:
                "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
                    .to_string(),
            purpose: "OpenAI-compatible-only provider policy keeps the language/no-thinking contract while preserving tool-call authority whenever the current lifecycle has open obligations".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::PromptReplayAuthority,
            fixture_id:
                "fixture.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.lifecycle_kernel.turn_lifecycle_plan_authority".to_string(),
            purpose: "kernel-owned TurnLifecyclePlan decides dispatch tool_choice, replay policy, proposal policy, corrective policy, terminal policy, continuation expectation, and diagnostics projection from stable surface and typed recovery state before TurnRuntime sends a provider request".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.lifecycle_kernel.turn_lifecycle_plan_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.typed_route_metadata_authority".to_string(),
            purpose: "tool lifecycle reads requested/effective tool, allowed surface, permission, sandbox, retry, and terminal guard from typed metadata".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.typed_route_metadata_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.rejected_singleton_payload_terminal_guard"
                .to_string(),
            purpose: "rejected singleton write payloads are call-id-scoped corrective outputs and terminalize repeated no-progress before route timeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            fixture_id: "fixture.tool_lifecycle.rejected_singleton_payload_terminal_guard"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.pre_execution_corrective_order_authority"
                .to_string(),
            purpose: "ToolLifecycleRuntime owns pre-execution corrective result ordering before filesystem or shell side effects, including repair target authority before verification command rejection".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.pre_execution_corrective_order_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.no_content_write_is_no_progress"
                .to_string(),
            purpose: "no-content write outputs and destructive no-op acknowledgement patches are typed no-progress / rejected results and cannot satisfy verification repair or authoring progress".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            fixture_id: "fixture.tool_lifecycle.no_content_write_is_no_progress"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.active_authoring_rejects_wrong_target"
                .to_string(),
            purpose: "content-changing write/apply_patch calls under requested-work authoring only count as progress when submitted targets intersect the current active deliverable set; repeated outside-target calls terminalize before route timeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id:
                "fixture.tool_lifecycle.active_authoring_rejects_wrong_target"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
                .to_string(),
            purpose: "repair-required active work remains edit authority even when stale continuation or prompt candidate surface exposes shell only".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ControlEnvelopeProjection,
            fixture_id: "fixture.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.executed_failure_call_output_terminal_guard"
                .to_string(),
            purpose: "executed tool failures remain call-id-scoped failed outputs and repeated same failures terminalize before route timeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.executed_failure_call_output_terminal_guard"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.verification_stable_tool_surface"
                .to_string(),
            purpose: "verification-only obligations narrow provider-visible action authority to the exact shell verification command while preserving edit surface when repair is required".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.verification_stable_tool_surface"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.authoring_stable_tool_surface"
                .to_string(),
            purpose: "requested-work authoring obligations constrain content-changing satisfaction while preserving file-changing tools and saturating plan-only progress projection after no-progress context".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.authoring_stable_tool_surface"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.progress_projection_stable_surface_guard"
                .to_string(),
            purpose: "progress projection remains a stable provider-visible tool and repeated plan-only output is guarded by call-scoped no-progress evidence instead of schema mutation".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.progress_projection_stable_surface_guard"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.edit_surface_registry_symmetry"
                .to_string(),
            purpose: "core edit tools remain provider-visible when runtime can dispatch them, while failed inactive write feedback stays call-id-scoped as a ToolCall/ToolOutput pair".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.edit_surface_registry_symmetry"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"
                .to_string(),
            purpose: "repeated disallowed or malformed known-tool feedback uses semantic lifecycle keys; repair-required stale verification reruns and forbidden supporting reads are classified before generic unavailable-tool feedback, then terminalize before outer timeout".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.rejected_tool_semantic_terminal_guard"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.synthetic_feedback_not_verification_authority"
                .to_string(),
            purpose: "synthetic corrective and unavailable-tool feedback stay in tool lifecycle authority and cannot become executed verification evidence".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id:
                "fixture.tool_lifecycle.synthetic_feedback_not_verification_authority"
                    .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.workspace_relative_file_change_authority"
                .to_string(),
            purpose: "file change evidence stores workspace-relative paths so closeout/reducer authority cannot mix route-root and session cwd coordinate systems".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.workspace_relative_file_change_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.shell_mutation_syncs_edit_baseline"
                .to_string(),
            purpose: "workspace file mutations detected after shell execution are synchronized into the same confirmed-content baseline used by write/apply_patch, while deleted paths are removed from that baseline".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.shell_mutation_syncs_edit_baseline"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.shell_output_encoding_authority"
                .to_string(),
            purpose: "shell stdout/stderr display projection preserves UTF-8 and Japanese Windows legacy code-page output, and Windows child Python processes are forced toward UTF-8 text mode".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.shell_output_encoding_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.command_text_encoding_contract"
                .to_string(),
            purpose: "shell execution performs a generic command text I/O encoding review before side effects, distinguishing explicit encoding control from hidden tool-owned bootstrap inheritance".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.command_text_encoding_contract"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.shell_timeout_process_tree_authority"
                .to_string(),
            purpose: "shell timeout and cancellation terminate the descendant process tree before killing the parent shell, so tool lifecycle completion cannot leave orphaned workspace commands running".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.shell_timeout_process_tree_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.closed_network_shell_authority"
                .to_string(),
            purpose: "shell execution requires mandatory user review for dependency installation, environment bootstrap, runtime download, and external network commands before side effects, while command stdout/stderr/exit code remain visible with retry guidance for malformed local commands".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.closed_network_shell_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.vision.input_item_lifecycle_authority".to_string(),
            purpose: "vision attachments are provider-visible labeled image items while local source paths remain diagnostics/UI metadata, not workspace file authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ProtocolItemLifecycle,
            fixture_id: "fixture.vision.input_item_lifecycle_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.workspace.absolute_turn_cwd_root_authority".to_string(),
            purpose: "app-boundary workspace discovery resolves relative run directories to absolute cwd/root authority before tool execution".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.workspace.absolute_turn_cwd_root_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.lifecycle_kernel.provider_noncompliance_adjudication"
                .to_string(),
            purpose: "OpenAI-compatible provider tool proposals outside the compiled TurnControlEnvelope surface, or with malformed arguments, are adapted into typed provider noncompliance lifecycle evidence before any tool execution".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            fixture_id: "fixture.lifecycle_kernel.provider_noncompliance_adjudication"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.turn_decision.codex_stable_tool_surface_authority"
                .to_string(),
            purpose: "legacy requested-action strings cannot narrow candidate tool surfaces, mutate schemas, or force tool choice outside the Codex-style tool-call lifecycle".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ControlEnvelopeProjection,
            fixture_id: "fixture.turn_decision.codex_stable_tool_surface_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.turn_decision.active_work_edit_before_verification_rerun"
                .to_string(),
            purpose: "source-owned verification failure compiles edit-required active work into prompt guidance and tool lifecycle validation before rerun, while repair-lane projection remains diagnostic and cannot become a second dispatch root".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ControlEnvelopeProjection,
            fixture_id: "fixture.turn_decision.active_work_edit_before_verification_rerun"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.closeout.final_assistant_message_lifecycle".to_string(),
            purpose: "clean closeout and no-executable-work answer-only turns use assistant message completion with no provider-visible synthetic completion tool, while closeout-ready final-only provider waits are bounded by a terminal guard".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ControlEnvelopeProjection,
            fixture_id: "fixture.closeout.final_assistant_message_lifecycle".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.closeout.open_obligation_final_assistant_continuation_hook"
                .to_string(),
            purpose: "runtime-completed final assistant messages remain Codex item lifecycle events, while app/harness closeout with open obligations records an explicit continuation user turn before bounded route failure".to_string(),
            tier: 2,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ManualStEvidenceSchema,
            fixture_id: "fixture.closeout.open_obligation_final_assistant_continuation_hook"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.closeout.verification_failure_preserves_closeout_evidence"
                .to_string(),
            purpose: "manual ST route verification failures preserve Codex-style closeout evidence instead of short-circuiting before open obligation and missing artifact evidence is materialized".to_string(),
            tier: 2,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ManualStEvidenceSchema,
            fixture_id: "fixture.closeout.verification_failure_preserves_closeout_evidence"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.closeout.verification_repair_continuation_hook".to_string(),
            purpose: "failed required verification after a runtime-completed final assistant item records a typed text-only verification-repair hook prompt with repair target, failed command, failure evidence, edit requirement, and rerun condition".to_string(),
            tier: 2,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ManualStEvidenceSchema,
            fixture_id: "fixture.closeout.verification_repair_continuation_hook".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.closeout.verification_labels_not_requested_work".to_string(),
            purpose: "failed verification labels and generated test method symbols remain evidence labels; they cannot become requested-work authoring targets or no-tool closeout authority while failed verification repair remains open".to_string(),
            tier: 2,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ManualStEvidenceSchema,
            fixture_id: "fixture.closeout.verification_labels_not_requested_work".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.verification.typed_evidence_cluster_authority".to_string(),
            purpose: "verification repair authority is derived from VerificationFailureCluster evidence rather than raw summary text".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            fixture_id: "fixture.verification.typed_evidence_cluster_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.desktop_transcript.completed_primary_reading_path".to_string(),
            purpose: "Desktop and history Markdown projection renders chronological user-turn blocks with folded work evidence, typed terminal outcome authority, final closeout, and typed file-change rows".to_string(),
            tier: 2,
            layer: PreflightLayer::QualityGate,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesktopTranscriptProjectionAuthority,
            fixture_id: "fixture.desktop_transcript.completed_primary_reading_path".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.route_evidence.schema".to_string(),
            purpose: "manual ST entry requires route-level harness-owned evidence artifacts before representative rerun".to_string(),
            tier: 3,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ManualStEvidenceSchema,
            fixture_id: "fixture.manual_st.route_evidence_schema".to_string(),
        },
    ]
}

pub fn default_preflight_fixtures() -> Vec<PreflightFixture> {
    vec![
        PreflightFixture {
            fixture_id: "fixture.protocol.history_item_lifecycle_authority".to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "runtime_event_stream explicit_turn_started turn_context_authority turn_item_stream terminal_turn_event canonical_history_item_stream typed_tool_arguments typed_file_change_evidence typed_tool_output_success".to_string(),
            required_refs: vec![
                "runtime_event_stream".to_string(),
                "explicit_turn_started".to_string(),
                "turn_context_authority".to_string(),
                "terminal_turn_event".to_string(),
                "canonical_history_item_stream".to_string(),
                "turn_item_stream".to_string(),
                "typed_tool_arguments".to_string(),
                "typed_file_change_evidence".to_string(),
                "typed_tool_output_success".to_string(),
            ],
            forbidden_refs: vec![
                "raw_transcript_authority".to_string(),
                "materialized_view_authority".to_string(),
                "summary_parser_authority".to_string(),
                "latest_user_row_turn_boundary".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.protocol.persistence_unit_of_work_authority".to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "SqliteRuntimeUnitOfWork compatibility_message_persistence session_status_transition protocol_runtime_projection single_transaction terminal_event_last token_accounting_before_terminal emit_pre_recorded_after_commit".to_string(),
            required_refs: vec![
                "SqliteRuntimeUnitOfWork".to_string(),
                "compatibility_message_persistence".to_string(),
                "session_status_transition".to_string(),
                "protocol_runtime_projection".to_string(),
                "single_transaction".to_string(),
                "terminal_event_last".to_string(),
                "token_accounting_before_terminal".to_string(),
                "emit_pre_recorded_after_commit".to_string(),
            ],
            forbidden_refs: vec![
                "transcript_only_projection".to_string(),
                "display_event_before_persistence".to_string(),
                "separate_status_projection_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.item_lifecycle.provider_replay_call_output_symmetry"
                .to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "canonical_history_item_stream provider_replay call_id_scoped_tool_output missing_output_aborted orphan_output_omitted runtime_error_excluded transcript_projection_display_only".to_string(),
            required_refs: vec![
                "canonical_history_item_stream".to_string(),
                "provider_replay".to_string(),
                "call_id_scoped_tool_output".to_string(),
                "missing_output_aborted".to_string(),
                "orphan_output_omitted".to_string(),
                "runtime_error_excluded".to_string(),
                "transcript_projection_display_only".to_string(),
            ],
            forbidden_refs: vec![
                "tool_output_as_assistant_text".to_string(),
                "runtime_error_as_assistant_history".to_string(),
                "raw_transcript_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.llm_transport.stream_retry_before_first_event".to_string(),
            family: PreflightGateFamily::LlmTransportAuthority,
            authority_source: "ProviderStreamRetry stream_max_retries request_diagnostics_stream_max_retries stream_retry_exhausted_terminal_evidence sse_transport_error body_decode_error stream_idle_timeout retry_before_first_model_event no_retry_after_partial_model_event no_retry_for_parse_or_provider_error".to_string(),
            required_refs: vec![
                "ProviderStreamRetry".to_string(),
                "stream_max_retries".to_string(),
                "request_diagnostics_stream_max_retries".to_string(),
                "stream_retry_exhausted_terminal_evidence".to_string(),
                "sse_transport_error".to_string(),
                "body_decode_error".to_string(),
                "stream_idle_timeout".to_string(),
                "retry_before_first_model_event".to_string(),
                "no_retry_after_partial_model_event".to_string(),
                "no_retry_for_parse_or_provider_error".to_string(),
            ],
            forbidden_refs: vec![
                "retry_after_partial_output".to_string(),
                "stream_error_always_terminal".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.llm_transport.streaming_timeout_boundary".to_string(),
            family: PreflightGateFamily::LlmTransportAuthority,
            authority_source: "ProviderStreamingTimeout request_timeout_ms response_header_timeout stream_idle_timeout_ms stream_body_idle_timeout no_total_body_timeout long_running_stream_allowed".to_string(),
            required_refs: vec![
                "ProviderStreamingTimeout".to_string(),
                "request_timeout_ms".to_string(),
                "response_header_timeout".to_string(),
                "stream_idle_timeout_ms".to_string(),
                "stream_body_idle_timeout".to_string(),
                "no_total_body_timeout".to_string(),
                "long_running_stream_allowed".to_string(),
            ],
            forbidden_refs: vec![
                "request_timeout_as_total_body_timeout".to_string(),
                "stream_idle_timeout_shadowed".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.control_envelope.dispatch_projection_authority".to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "TurnControlEnvelope ProjectionBundle ActionAuthority allowed_surface tool_choice availability_metadata satisfying_file_change_progress_surface".to_string(),
            required_refs: vec![
                "TurnControlEnvelope".to_string(),
                "ProjectionBundle".to_string(),
                "ActionAuthority".to_string(),
                "availability_metadata".to_string(),
                "satisfying_file_change_progress_surface".to_string(),
            ],
            forbidden_refs: vec!["active_work_required_action_fallback".to_string(), "required_action_string_grammar".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.runtime_feedback_classification_authority"
                .to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "RecoverableRuntimeFeedback CompletionDrift ToolFeedbackEnvelope typed_work_remains repair_lane_absent typed_verification_evidence_required".to_string(),
            required_refs: vec![
                "RecoverableRuntimeFeedback".to_string(),
                "CompletionDrift".to_string(),
                "ToolFeedbackEnvelope".to_string(),
                "typed_work_remains".to_string(),
                "repair_lane_absent".to_string(),
                "typed_verification_evidence_required".to_string(),
            ],
            forbidden_refs: vec![
                "runtime_feedback_as_verification_failure".to_string(),
                "summary_parser_verification_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.requested_work_completion_promotes_verification"
                .to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "RequestedWorkAuthoring ReferenceInput reference_input_not_pending_deliverable scenario_contract_reference_input_not_authoring_target japanese_prompt_filename_token_boundary docs_output_referenced_code_not_pending_deliverable structured_document_summary_remaining_sources_block_closeout structured_document_summary_output_heading_progress_after_compaction no_verification_requested_work_file_change_closeout relative_workspace_root_absolute_filechange_progress escaped_windows_absolute_filechange_progress FileChangeEvidence authoring_complete Verification verification_command_obligation before_closeout canonical_item_chronology turn_local_sequence_no latest_content_change_invalidates_prior_verification VerificationRunResult passed_verification_command_consumed clean_closeout".to_string(),
            required_refs: vec![
                "RequestedWorkAuthoring".to_string(),
                "ReferenceInput".to_string(),
                "reference_input_not_pending_deliverable".to_string(),
                "scenario_contract_reference_input_not_authoring_target".to_string(),
                "japanese_prompt_filename_token_boundary".to_string(),
                "docs_output_referenced_code_not_pending_deliverable".to_string(),
                "structured_document_summary_remaining_sources_block_closeout".to_string(),
                "structured_document_summary_output_heading_progress_after_compaction".to_string(),
                "no_verification_requested_work_file_change_closeout".to_string(),
                "relative_workspace_root_absolute_filechange_progress".to_string(),
                "escaped_windows_absolute_filechange_progress".to_string(),
                "FileChangeEvidence".to_string(),
                "authoring_complete".to_string(),
                "Verification".to_string(),
                "verification_command_obligation".to_string(),
                "canonical_item_chronology".to_string(),
                "turn_local_sequence_no".to_string(),
                "latest_content_change_invalidates_prior_verification".to_string(),
                "VerificationRunResult".to_string(),
                "passed_verification_command_consumed".to_string(),
                "clean_closeout".to_string(),
            ],
            forbidden_refs: vec![
                "verification_command_log_empty_closeout".to_string(),
                "closeout_before_verification".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.post_repair_edit_promotes_verification_rerun"
                .to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "RepairControlSnapshot FileChangeEvidence content_changing_repair_progress target_normalization source_owned_generated_test_evidence_cleanup Verification exact_shell_rerun runtime_owned_required_verification_dispatch before_repair_reissue".to_string(),
            required_refs: vec![
                "RepairControlSnapshot".to_string(),
                "FileChangeEvidence".to_string(),
                "content_changing_repair_progress".to_string(),
                "target_normalization".to_string(),
                "source_owned_generated_test_evidence_cleanup".to_string(),
                "Verification".to_string(),
                "exact_shell_rerun".to_string(),
                "runtime_owned_required_verification_dispatch".to_string(),
            ],
            forbidden_refs: vec![
                "stale_repair_target_after_successful_write".to_string(),
                "evidence_only_generated_test_target_after_source_repair_progress".to_string(),
                "repair_lane_top_level_required_action_mismatch".to_string(),
                "repair_lane_top_level_target_mismatch".to_string(),
                "source_owned_active_work_generated_test_evidence_target".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id:
                "fixture.state_reducer.verification_failure_preserves_repair_target_authority"
                    .to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "CodexHistoryItemStream session_state_projection_not_sequence_floor VerificationRunResult VerificationFailureCluster VerificationFailureEvidence public_output_stream_assertion_mismatch public_command_contract_failure_projection active_obligation_targets source_owned_repair_control_snapshot source_owned_recent_file_change_target_preserved source_owned_repair_test_to_source_target_normalization source_owned_active_work_exact_target_projection source_owned_public_output_stream_active_work_exact_target_projection source_owned_requirement_refs_align_active_work_with_repair_lane contract_visible_public_exception_owner_authority generated_test_constructor_api_misuse_owner_authority generated_test_parse_defect_owner_authority generated_test_reflection_api_misuse_owner_authority generated_test_module_attribute_api_misuse_owner_authority generated_test_exception_type_overreach_owner_authority source_parse_defect_owner_authority generated_test_name_resolution_owner_authority generated_test_import_nameerror_owner_authority mixed_source_test_contract_reconciliation_owner_authority generated_test_contract_overreach_owner_projection_alignment generic_generated_test_only_owner_target_authority ungrounded_generated_public_output_assertion_owner_authority generated_test_local_binding_contradiction_owner_authority source_constructor_mismatch_counterexample verification_timeout_recent_source_target_preserved targetless_unclassified_repair_dispatch_blocked verification_labels_not_file_targets python_runtime_traceback_frames_excluded import_error_module_target_authority diagnostic_scalar_values_are_not_repair_targets".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "session_state_projection_not_sequence_floor".to_string(),
                "VerificationRunResult".to_string(),
                "VerificationFailureCluster".to_string(),
                "VerificationFailureEvidence".to_string(),
                "public_output_stream_assertion_mismatch".to_string(),
                "public_command_contract_failure_projection".to_string(),
                "active_obligation_targets".to_string(),
                "source_owned_repair_control_snapshot".to_string(),
                "source_owned_recent_file_change_target_preserved".to_string(),
                "source_owned_repair_test_to_source_target_normalization".to_string(),
                "source_owned_active_work_exact_target_projection".to_string(),
                "source_owned_public_output_stream_active_work_exact_target_projection"
                    .to_string(),
                "source_owned_requirement_refs_align_active_work_with_repair_lane"
                    .to_string(),
                "contract_visible_public_exception_owner_authority".to_string(),
                "generated_test_constructor_api_misuse_owner_authority".to_string(),
                "generated_test_parse_defect_owner_authority".to_string(),
                "generated_test_reflection_api_misuse_owner_authority".to_string(),
                "generated_test_module_attribute_api_misuse_owner_authority".to_string(),
                "generated_test_exception_type_overreach_owner_authority".to_string(),
                "source_parse_defect_owner_authority".to_string(),
                "generated_test_contract_overreach_owner_projection_alignment".to_string(),
                "generic_generated_test_only_owner_target_authority".to_string(),
                "ungrounded_generated_public_output_assertion_owner_authority".to_string(),
                "generated_test_name_resolution_owner_authority".to_string(),
                "generated_test_import_nameerror_owner_authority".to_string(),
                "mixed_source_test_contract_reconciliation_owner_authority".to_string(),
                "generated_test_local_binding_contradiction_owner_authority".to_string(),
                "source_constructor_mismatch_counterexample".to_string(),
                "verification_timeout_recent_source_target_preserved".to_string(),
                "targetless_unclassified_repair_dispatch_blocked".to_string(),
                "verification_labels_not_file_targets".to_string(),
                "python_runtime_traceback_frames_excluded".to_string(),
                "import_error_module_target_authority".to_string(),
                "diagnostic_scalar_values_are_not_repair_targets".to_string(),
            ],
            forbidden_refs: vec![
                "session_state_sequence_floor".to_string(),
                "empty_active_targets_after_verification_failure".to_string(),
                "repair_unclassified_dispatched_with_broad_surface".to_string(),
                "stdlib_unittest_loader_frame_as_repair_target".to_string(),
                "source_owned_repair_generated_test_exact_target".to_string(),
                "source_owned_active_work_generated_test_evidence_target".to_string(),
                "repair_lane_exact_target_outside_active_work".to_string(),
                "diagnostic_scalar_value_as_repair_target".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.docs_route_contract_authority".to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "CodexHistoryItemStream TaskRoute::Docs DocsRouteState DocsRepair route_contract_pending route_contract_satisfied_typed_closeout docs_only_mutation_boundary docs_route_closeout_continuation_preserves_docs_authority same_document_docs_update_route_authority same_document_update_requires_latest_file_change prior_authored_document_update_target dynamic_docs_area_contract flat_test_artifact_area_coverage generated_dependency_evidence_excluded RequestedWorkAuthoring_not_primary localized_docs_topic_completion docs_route_semantic_no_progress_guard docs_spec_semantic_reconciliation_no_progress_terminal_guard docs_supporting_context_budget_exhausted_corrective_tool_output docs_budget_exhausted_recovery_surface_narrowed docs_budget_exhaustion_survives_partial_write supporting_context_evidence_survives_surface_narrowing docs_content_grounding_before_exact_write_recovery docs_required_topic_content_grounding docs_completed_deliverable_regression_rejected write_ready_prompt_projection docs_spec_semantic_reconciliation".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "TaskRoute::Docs".to_string(),
                "DocsRouteState".to_string(),
                "DocsRepair".to_string(),
                "route_contract_pending".to_string(),
                "route_contract_satisfied_typed_closeout".to_string(),
                "docs_only_mutation_boundary".to_string(),
                "same_document_docs_update_route_authority".to_string(),
                "same_document_update_requires_latest_file_change".to_string(),
                "prior_authored_document_update_target".to_string(),
                "flat_test_artifact_area_coverage".to_string(),
                "generated_dependency_evidence_excluded".to_string(),
                "localized_docs_topic_completion".to_string(),
                "docs_route_semantic_no_progress_guard".to_string(),
                "docs_route_closeout_continuation_preserves_docs_authority".to_string(),
                "docs_spec_semantic_reconciliation_no_progress_terminal_guard".to_string(),
                "docs_supporting_context_budget_exhausted_corrective_tool_output".to_string(),
                "docs_budget_exhausted_recovery_surface_narrowed".to_string(),
                "docs_budget_exhaustion_survives_partial_write".to_string(),
                "supporting_context_evidence_survives_surface_narrowing".to_string(),
                "docs_content_grounding_before_exact_write_recovery".to_string(),
                "docs_required_topic_content_grounding".to_string(),
                "docs_completed_deliverable_regression_rejected".to_string(),
                "write_ready_prompt_projection".to_string(),
                "docs_spec_semantic_reconciliation".to_string(),
            ],
            forbidden_refs: vec![
                "case5_primary_key".to_string(),
                "filename_only_requested_work_authoring".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.docs_spec.semantic_reconciliation_before_handoff".to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "DocsSpecSemanticContract latest_user_request_authority required_claims prohibited_claims semantic_reconciliation_before_handoff side_effect_free_corrective_tool_output no_file_change_progress_on_contradiction prompt_visible_reconciliation_contract docs_semantic_reconciliation_feedback_projection docs_semantic_prohibited_claim_requires_affirmative_occurrence docs_spec_semantic_reconciliation_no_progress_terminal_guard semantic_claim_key_payload_independent".to_string(),
            required_refs: vec![
                "DocsSpecSemanticContract".to_string(),
                "latest_user_request_authority".to_string(),
                "required_claims".to_string(),
                "prohibited_claims".to_string(),
                "semantic_reconciliation_before_handoff".to_string(),
                "side_effect_free_corrective_tool_output".to_string(),
                "no_file_change_progress_on_contradiction".to_string(),
                "prompt_visible_reconciliation_contract".to_string(),
                "docs_semantic_reconciliation_feedback_projection".to_string(),
                "docs_semantic_prohibited_claim_requires_affirmative_occurrence".to_string(),
                "docs_spec_semantic_reconciliation_no_progress_terminal_guard".to_string(),
                "semantic_claim_key_payload_independent".to_string(),
            ],
            forbidden_refs: vec![
                "case3_primary_key".to_string(),
                "calculator_design_primary_key".to_string(),
                "model_visible_string_only_fix".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.verification.public_command_contract_coverage".to_string(),
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            authority_source: "PublicCommandContract prompt_visible_command_examples generated_test_subprocess_coverage argv_tokens expected_exit_code stdout_stderr_observation stdout_line_suffix_delimited_suffix route_verification_evidence route_verification_utf8_process_environment expected_nonzero_pass_allowed delta_edit_post_patch_candidate_projection parent_child_encoding_alignment subprocess_timeout_authority subprocess_output_capture_authority public_command_contract_issue_kind public_command_contract_failure_projection compact_route_failure_evidence source_public_command_contract encoding_contract_issues subprocess_timeout_contract_issues subprocess_output_capture_contract_issues".to_string(),
            required_refs: vec![
                "PublicCommandContract".to_string(),
                "prompt_visible_command_examples".to_string(),
                "generated_test_subprocess_coverage".to_string(),
                "argv_tokens".to_string(),
                "expected_exit_code".to_string(),
                "stdout_stderr_observation".to_string(),
                "stdout_line_suffix_delimited_suffix".to_string(),
                "route_verification_evidence".to_string(),
                "route_verification_utf8_process_environment".to_string(),
                "expected_nonzero_pass_allowed".to_string(),
                "delta_edit_post_patch_candidate_projection".to_string(),
                "parent_child_encoding_alignment".to_string(),
                "subprocess_timeout_authority".to_string(),
                "subprocess_output_capture_authority".to_string(),
                "public_command_contract_issue_kind".to_string(),
                "public_command_contract_failure_projection".to_string(),
                "source_public_command_contract".to_string(),
                "encoding_contract_issues".to_string(),
                "subprocess_timeout_contract_issues".to_string(),
                "subprocess_output_capture_contract_issues".to_string(),
            ],
            forbidden_refs: vec![
                "case3_primary_key".to_string(),
                "calculator_primary_key".to_string(),
                "unittest_only_oracle".to_string(),
                "python_command_as_gate_primary_key".to_string(),
                "raw_apply_patch_payload_as_full_artifact".to_string(),
                "route_verification_platform_default_text_io".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.verification.command_correction_satisfies_obligation"
                .to_string(),
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            authority_source: "VerificationRunResult original_command effective_command satisfies_command_identities command_correction_alias required_verification_obligation_consumption command_identity_deduplication".to_string(),
            required_refs: vec![
                "VerificationRunResult".to_string(),
                "satisfies_command_identities".to_string(),
                "command_correction_alias".to_string(),
                "required_verification_obligation_consumption".to_string(),
                "command_identity_deduplication".to_string(),
            ],
            forbidden_refs: vec![
                "python_unittest_primary_key".to_string(),
                "powershell_wrapper_primary_key".to_string(),
                "raw_command_string_equality_only".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.plan_progress_projection.todo_absence_does_not_gate_authoring"
                .to_string(),
            family: PreflightGateFamily::PlanProgressProjectionAuthority,
            authority_source: "CodexPlanTool ProgressProjection RequestedWorkAuthoring write_authority todo_absence_nonblocking progress_projection_not_work_progress progress_side_channel_not_first_artifact_action open_work_terminal_guard".to_string(),
            required_refs: vec![
                "CodexPlanTool".to_string(),
                "ProgressProjection".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "write_authority".to_string(),
                "todo_absence_nonblocking".to_string(),
                "progress_projection_not_work_progress".to_string(),
                "progress_side_channel_not_first_artifact_action".to_string(),
                "open_work_terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "todo_graph_as_authoring_gate".to_string(),
                "progress_projection_required_before_write".to_string(),
                "progress_projection_satisfies_artifact_work".to_string(),
                "turn_step_budget_plan_only_loop".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.stale_write_arguments_summary_projection"
                .to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection current_lifecycle_state current_todo_focus_projection write_content_test_contract stable_provider_tool_schema provider_owned_tool_arguments final_dispatch_source_schema_projection positive_test_module_shape_contract executable_test_module_shape_contract test_class_base_contract string_literal_test_module_rejected source_executable_artifact_shape_contract escaped_source_string_rejected escaped_source_write_candidate_normalized source_test_module_payload_rejected corrective_content_shape_no_progress_terminal_guard python_source_repair_positive_contract text_artifact_readable_content_shape serialized_markdown_rejected text_artifact_repair_positive_contract content_shape_workspace_target_normalization consumed_supporting_context_replay_omitted post_patch_test_module_shape_contract observed_forbidden_marker_feedback unittest_main_test_content_allowed failed_write_content_shape_nonprogress sanitized_failed_write_tool_call_lifecycle summary_only_tool_call_replay omitted_corrective_output_latest_recovery stale_arguments_suppressed stale_payload_omitted stale_prelude_omitted stale_todo_progress_replay_omitted internal_control_items_not_provider_visible".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "current_lifecycle_state".to_string(),
                "current_todo_focus_projection".to_string(),
                "write_content_test_contract".to_string(),
                "stable_provider_tool_schema".to_string(),
                "final_dispatch_source_schema_projection".to_string(),
                "summary_only_tool_call_replay".to_string(),
                "provider_owned_tool_arguments".to_string(),
                "provider_owned_tool_arguments".to_string(),
                "write_content_test_contract".to_string(),
                "positive_test_module_shape_contract".to_string(),
                "executable_test_module_shape_contract".to_string(),
                "test_class_base_contract".to_string(),
                "string_literal_test_module_rejected".to_string(),
                "source_executable_artifact_shape_contract".to_string(),
                "escaped_source_string_rejected".to_string(),
                "escaped_source_write_candidate_normalized".to_string(),
                "source_test_module_payload_rejected".to_string(),
                "corrective_content_shape_no_progress_terminal_guard".to_string(),
                "python_source_repair_positive_contract".to_string(),
                "text_artifact_readable_content_shape".to_string(),
                "serialized_markdown_rejected".to_string(),
                "text_artifact_repair_positive_contract".to_string(),
                "consumed_supporting_context_replay_omitted".to_string(),
                "content_shape_workspace_target_normalization".to_string(),
                "post_patch_test_module_shape_contract".to_string(),
                "observed_forbidden_marker_feedback".to_string(),
                "unittest_main_test_content_allowed".to_string(),
                "failed_write_content_shape_nonprogress".to_string(),
                "sanitized_failed_write_tool_call_lifecycle".to_string(),
                "omitted_corrective_output_latest_recovery".to_string(),
                "stale_arguments_suppressed".to_string(),
                "stale_payload_omitted".to_string(),
                "stale_prelude_omitted".to_string(),
                "stale_todo_progress_replay_omitted".to_string(),
                "internal_control_items_not_provider_visible".to_string(),
            ],
            forbidden_refs: vec![
                "stale_write_arguments_authority".to_string(),
                "todowrite_history_as_action_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.active_user_hook_non_droppable".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection current_user_input_non_droppable hook_prompt_non_droppable trailing_compaction_summary reducible_older_tool_outputs CompactionContinuity".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "current_user_input_non_droppable".to_string(),
                "hook_prompt_non_droppable".to_string(),
                "trailing_compaction_summary".to_string(),
                "reducible_older_tool_outputs".to_string(),
                "CompactionContinuity".to_string(),
            ],
            forbidden_refs: vec![
                "compaction_item_as_hard_drop_boundary".to_string(),
                "latest_user_prompt_omitted".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.tool_pair_symmetry".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection call_id_scoped_tool_call_output_pair model_arguments_replay_authority effective_tool_surface_scoped_replay intermediate_assistant_text_omitted_while_open rejected_final_message_no_progress_evidence current_malformed_edit_arguments_sanitized invalid_edit_arguments_output_preserved no_orphan_tool_output latest_user_input_preserved".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "call_id_scoped_tool_call_output_pair".to_string(),
                "model_arguments_replay_authority".to_string(),
                "effective_tool_surface_scoped_replay".to_string(),
                "intermediate_assistant_text_omitted_while_open".to_string(),
                "rejected_final_message_no_progress_evidence".to_string(),
                "current_malformed_edit_arguments_sanitized".to_string(),
                "invalid_edit_arguments_output_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
                "latest_user_input_preserved".to_string(),
            ],
            forbidden_refs: vec![
                "standalone_tool_output_without_call".to_string(),
                "provider_tool_role_without_assistant_tool_call".to_string(),
                "arguments_null_drops_tool_call".to_string(),
                "raw_malformed_edit_payload_replayed".to_string(),
                "invalid_edit_arguments_output_omitted".to_string(),
                "historical_tool_call_outside_effective_surface".to_string(),
                "unaccepted_assistant_text_as_completion_authority".to_string(),
                "rejected_final_message_item_omitted".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.compaction_orphan_assistant_repaired".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection CompactionContinuity post_compaction_role_alternation latest_user_input_preserved matching_user_query_restored_before_assistant compaction_trigger_ignores_pre_summary_history".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "CompactionContinuity".to_string(),
                "post_compaction_role_alternation".to_string(),
                "latest_user_input_preserved".to_string(),
                "matching_user_query_restored_before_assistant".to_string(),
                "compaction_trigger_ignores_pre_summary_history".to_string(),
            ],
            forbidden_refs: vec![
                "assistant_message_without_matching_user_after_compaction".to_string(),
                "provider_template_no_user_query".to_string(),
                "pre_summary_history_token_pressure_retrigger".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.stale_inactive_authoring_pair_omitted"
                .to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection stale_inactive_authoring_pair_omitted non_executable_history_summary reference_only_inactive_artifact_snapshot inactive_filechange_without_replayable_tool_call_snapshot no_fake_executable_tool_arguments current_active_target_preserved no_orphan_tool_output system_context_top_level_only".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "stale_inactive_authoring_pair_omitted".to_string(),
                "non_executable_history_summary".to_string(),
                "reference_only_inactive_artifact_snapshot".to_string(),
                "inactive_filechange_without_replayable_tool_call_snapshot".to_string(),
                "no_fake_executable_tool_arguments".to_string(),
                "current_active_target_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
                "system_context_top_level_only".to_string(),
            ],
            forbidden_refs: vec![
                "[omitted inactive authoring target]".to_string(),
                "[omitted stale inactive authoring payload".to_string(),
                "sentinel_path_as_tool_argument".to_string(),
                "system_after_user_provider_message".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.progress_projection_pair_omitted".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection progress_projection_pair_omitted non_executable_planning_context current_progress_feedback_pair_preserved call_id_scoped_current_plan_output no_stale_todo_json current_active_target_preserved no_orphan_tool_output".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "progress_projection_pair_omitted".to_string(),
                "non_executable_planning_context".to_string(),
                "current_progress_feedback_pair_preserved".to_string(),
                "call_id_scoped_current_plan_output".to_string(),
                "no_stale_todo_json".to_string(),
                "current_active_target_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
            ],
            forbidden_refs: vec![
                "stale_todo_json_as_tool_argument".to_string(),
                "progress_projection_as_current_authoring_plan".to_string(),
                "current_plan_feedback_omitted".to_string(),
                "provider_tool_role_without_assistant_tool_call".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id:
                "fixture.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
                    .to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "OpenAICompatibleOnlyProviderPolicy PromptProjection language_policy no_thinking_policy tool_lifecycle_compatibility open_obligation_tool_authority final_assistant_after_obligations_only configured_tool_turn_output_budget configured_max_output_tokens effective_max_output_tokens output_budget_reason openai_compatible_system_authority_merge single_leading_system_message runtime_system_control_projection_merge".to_string(),
            required_refs: vec![
                "OpenAICompatibleOnlyProviderPolicy".to_string(),
                "PromptProjection".to_string(),
                "language_policy".to_string(),
                "no_thinking_policy".to_string(),
                "tool_lifecycle_compatibility".to_string(),
                "open_obligation_tool_authority".to_string(),
                "final_assistant_after_obligations_only".to_string(),
                "configured_tool_turn_output_budget".to_string(),
                "configured_max_output_tokens".to_string(),
                "effective_max_output_tokens".to_string(),
                "output_budget_reason".to_string(),
                "openai_compatible_system_authority_merge".to_string(),
                "single_leading_system_message".to_string(),
                "runtime_system_control_projection_merge".to_string(),
            ],
            forbidden_refs: vec![
                "provider_policy_overrides_tool_lifecycle".to_string(),
                "final_answer_only_with_open_obligations".to_string(),
                "multiple_system_message_authority_roots".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.typed_route_metadata_authority".to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolLifecycleEnvelope requested_tool effective_tool allowed_surface permission_decision sandbox_profile retry_policy terminal_guard".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "requested_tool".to_string(),
                "effective_tool".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec!["tool_result_summary_parser_authority".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.rejected_singleton_payload_terminal_guard"
                .to_string(),
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            authority_source: "ToolLifecycleEnvelope RejectedToolProposal FunctionCallOutput success=false call_id_scoped_corrective_output allowed_surface terminal_guard followup_visible_terminal_guard".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "RejectedToolProposal".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "call_id_scoped_corrective_output".to_string(),
                "allowed_surface".to_string(),
                "terminal_guard".to_string(),
                "followup_visible_terminal_guard".to_string(),
            ],
            forbidden_refs: vec!["tool_result_summary_parser_authority".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.pre_execution_corrective_order_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolLifecycleRuntime PreExecutionCorrectiveDecision PreExecutionCorrectiveKind artifact_content_shape_before_side_effect repair_target_authority before wrong_verification_shell_command wrong_authoring_target docs_semantic_reconciliation public_command_contract FunctionCallOutput success=false terminal_guard".to_string(),
            required_refs: vec![
                "ToolLifecycleRuntime".to_string(),
                "PreExecutionCorrectiveDecision".to_string(),
                "PreExecutionCorrectiveKind".to_string(),
                "artifact_content_shape_before_side_effect".to_string(),
                "repair_target_authority before wrong_verification_shell_command".to_string(),
                "wrong_authoring_target".to_string(),
                "docs_semantic_reconciliation".to_string(),
                "public_command_contract".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "TurnRuntimeCorrectiveOrder".to_string(),
                "case_specific_corrective_branch".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.no_content_write_is_no_progress"
                .to_string(),
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            authority_source: "ToolLifecycleEnvelope FunctionCallOutput success=false progress_effect=no_progress unambiguous_malformed_edit_argument_repair malformed_edit_argument_repair_projection empty_file_change_not_authoring_progress invalid_edit_arguments_control_recovery_projection invalid_edit_recovery_candidate_target_operation invalid_edit_recovery_uses_open_target_when_candidate_is_inactive mixed_target_apply_patch_active_hunk_evidence mixed_target_invalid_edit_recovery_projection invalid_edit_arguments_recovery_persists_across_final_message parser_error raw_argument_shape_hash allowed_surface_snapshot malformed_write_patch_capable_recovery_surface malformed_apply_patch_write_recovery_surface no_content_change destructive_noop_acknowledgement_patch_rejected empty_apply_patch_hunks_rejected hunkless_update_patch_rejected bare_markdown_update_body_rejected add_file_unprefixed_content_line_feedback zero_diff_patch_rejected artifact_preservation".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "progress_effect=no_progress".to_string(),
                "unambiguous_malformed_edit_argument_repair".to_string(),
                "malformed_edit_argument_repair_projection".to_string(),
                "empty_file_change_not_authoring_progress".to_string(),
                "invalid_edit_arguments_control_recovery_projection".to_string(),
                "invalid_edit_recovery_candidate_target_operation".to_string(),
                "invalid_edit_recovery_uses_open_target_when_candidate_is_inactive".to_string(),
                "mixed_target_apply_patch_active_hunk_evidence".to_string(),
                "mixed_target_invalid_edit_recovery_projection".to_string(),
                "invalid_edit_arguments_recovery_persists_across_final_message".to_string(),
                "parser_error".to_string(),
                "raw_argument_shape_hash".to_string(),
                "allowed_surface_snapshot".to_string(),
                "malformed_write_patch_capable_recovery_surface".to_string(),
                "malformed_apply_patch_write_recovery_surface".to_string(),
                "no_content_change".to_string(),
                "destructive_noop_acknowledgement_patch_rejected".to_string(),
                "empty_apply_patch_hunks_rejected".to_string(),
                "hunkless_update_patch_rejected".to_string(),
                "bare_markdown_update_body_rejected".to_string(),
                "add_file_unprefixed_content_line_feedback".to_string(),
                "zero_diff_patch_rejected".to_string(),
                "artifact_preservation".to_string(),
            ],
            forbidden_refs: vec![
                "duplicate_success_cache".to_string(),
                "repair_progress_from_no_content_write".to_string(),
                "noop_acknowledgement_as_content_progress".to_string(),
                "zero_diff_patch_as_content_progress".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.active_authoring_rejects_wrong_target"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring active_deliverable_targets workspace_path_coordinate_authority escaped_windows_absolute_target_matches_relative_deliverable workspace_prefix_boundary ActiveWorkContract::Verification repair_required RepairOperationTemplate exact_target write_admission source_owned_repair_generated_test_rewrite_rejected ToolLifecycleEnvelope FunctionCallOutput success=false wrong_authoring_target wrong_authoring_target_semantic_no_progress_key progress_effect=no_progress terminal_guard stable_tool_schema".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "active_deliverable_targets".to_string(),
                "workspace_path_coordinate_authority".to_string(),
                "escaped_windows_absolute_target_matches_relative_deliverable".to_string(),
                "workspace_prefix_boundary".to_string(),
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_authoring_target".to_string(),
                "wrong_authoring_target_semantic_no_progress_key".to_string(),
                "RepairOperationTemplate exact_target write_admission".to_string(),
                "source_owned_repair_generated_test_rewrite_rejected".to_string(),
                "progress_effect=no_progress".to_string(),
                "terminal_guard".to_string(),
                "stable_tool_schema".to_string(),
            ],
            forbidden_refs: vec![
                "schema_const_payload_injection".to_string(),
                "content_changing_progress_outside_active_target".to_string(),
                "generated_test_rewrite_for_source_owned_defect".to_string(),
                "outer_timeout_on_repeated_wrong_target_write".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.verification_stable_tool_surface"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexHistoryItemStream ActiveWorkContract::Verification exact_shell_verification_authority runtime_owned_verification_command_dispatch repair_required_edit_surface repair_active_shell_probe_uses_repair_target_authority shell_command_satisfaction FunctionCallOutput wrong_verification_command progress_effect=no_progress terminal_guard".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::Verification".to_string(),
                "exact_shell_verification_authority".to_string(),
                "runtime_owned_verification_command_dispatch".to_string(),
                "repair_required_edit_surface".to_string(),
                "repair_active_shell_probe_uses_repair_target_authority".to_string(),
                "shell_command_satisfaction".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_verification_command".to_string(),
                "progress_effect=no_progress".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "broad_context_surface_when_only_verification_pending".to_string(),
                "repair_required_verification_shell_only_surface".to_string(),
                "schema_const_command_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.authoring_stable_tool_surface"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring stable_tool_interface content_changing_satisfaction supporting_context progress_projection_saturation supporting_context_budget_recovery_surface authoring_supporting_context_target_grounding_read authoring_target_grounding_required FunctionCallOutput progress_effect=no_progress terminal_guard".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "stable_tool_interface".to_string(),
                "content_changing_satisfaction".to_string(),
                "supporting_context".to_string(),
                "progress_projection_saturation".to_string(),
                "supporting_context_budget_recovery_surface".to_string(),
                "authoring_supporting_context_target_grounding_read".to_string(),
                "authoring_target_grounding_required".to_string(),
                "FunctionCallOutput".to_string(),
                "progress_effect=no_progress".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "authoring_write_only_tool_interface".to_string(),
                "unavailable_list_during_authoring".to_string(),
                "schema_const_target_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.progress_projection_stable_surface_guard"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ActiveWorkContract::RequestedWorkAuthoring ActiveWorkContract::Verification progress_projection_call_output progress_projection_edit_recovery progress_projection_target_grounding_read docs_content_grounding_progress_projection_preserves_grounding_surface write_apply_patch_preserved authoring_supporting_context_budget_recovery_surface authoring_supporting_context_target_grounding_read multi_target_authoring_consumed_grounding_edit_recovery partial_target_grounding_remaining_target_authority singleton_missing_target_source_reference_or_create_authority stage_continuation_existing_target_grounding_read docs_existing_target_update_exact_read_grounding generated_test_consumed_source_reference_active_target_grounding repair_supporting_context_target_scoped_grounding source_repair_initial_target_grounding_survives_edit_narrowing source_repair_initial_grounding_precedes_edit_only_recovery failed_patch_context_mismatch_target_grounding patch_context_mismatch_recovery_augments_read_surface invalid_edit_recovery_exact_target_regrounding no_required_action_string stable_tool_schema semantic_no_progress_guard".to_string(),
            required_refs: vec![
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "ActiveWorkContract::Verification".to_string(),
                "progress_projection_call_output".to_string(),
                "progress_projection_edit_recovery".to_string(),
                "progress_projection_target_grounding_read".to_string(),
                "docs_content_grounding_progress_projection_preserves_grounding_surface"
                    .to_string(),
                "write_apply_patch_preserved".to_string(),
                "authoring_supporting_context_budget_recovery_surface".to_string(),
                "authoring_supporting_context_target_grounding_read".to_string(),
                "multi_target_authoring_consumed_grounding_edit_recovery".to_string(),
                "partial_target_grounding_remaining_target_authority".to_string(),
                "singleton_missing_target_source_reference_or_create_authority".to_string(),
                "stage_continuation_existing_target_grounding_read".to_string(),
                "docs_existing_target_update_exact_read_grounding".to_string(),
                "generated_test_consumed_source_reference_active_target_grounding".to_string(),
                "repair_supporting_context_target_scoped_grounding".to_string(),
                "source_repair_initial_target_grounding_survives_edit_narrowing".to_string(),
                "source_repair_initial_grounding_precedes_edit_only_recovery".to_string(),
                "failed_patch_context_mismatch_target_grounding".to_string(),
                "patch_context_mismatch_recovery_augments_read_surface".to_string(),
                "invalid_edit_recovery_exact_target_regrounding".to_string(),
                "stable_tool_schema".to_string(),
                "semantic_no_progress_guard".to_string(),
            ],
            forbidden_refs: vec![
                "legacy_required_action_string_field".to_string(),
                "schema_const_target_authority".to_string(),
                "untyped_tool_surface_suppression".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.edit_surface_registry_symmetry"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolRouter core_edit_tool_surface_registry_symmetry FunctionCallOutput wrong_authoring_target call_id_scoped_failed_inactive_write call_id_scoped_failed_inactive_apply_patch failed_inactive_tool_call_output_pair failed_inactive_argument_payload_omitted write_visible apply_patch_visible successful_stale_inactive_payload_summary_only".to_string(),
            required_refs: vec![
                "ToolRouter".to_string(),
                "core_edit_tool_surface_registry_symmetry".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_authoring_target".to_string(),
                "call_id_scoped_failed_inactive_write".to_string(),
                "call_id_scoped_failed_inactive_apply_patch".to_string(),
                "failed_inactive_tool_call_output_pair".to_string(),
                "failed_inactive_argument_payload_omitted".to_string(),
                "write_visible".to_string(),
                "apply_patch_visible".to_string(),
                "successful_stale_inactive_payload_summary_only".to_string(),
            ],
            forbidden_refs: vec![
                "anonymous_wrong_target_correction_only".to_string(),
                "hidden_core_tool".to_string(),
                "broad_write_saturation".to_string(),
                "failed_inactive_summary_only".to_string(),
                "failed_inactive_raw_payload_replay".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.rejected_tool_semantic_terminal_guard"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "RejectedToolProposal semantic_no_progress_key invalid_edit_recovery_semantic_no_progress_key tool_allowed_false tool_choice_auto broad_surface_terminal_guard rejected_tool_required_action_terminal_guard lifecycle_kernel_provider_noncompliance provider_ignored_edit_only_surface malformed_tool_arguments_terminal_guard result_hash_evidence_not_key repair_required_edit_before_verification_rerun argument_payload_omitted projection_noise_absent".to_string(),
            required_refs: vec![
                "RejectedToolProposal".to_string(),
                "semantic_no_progress_key".to_string(),
                "invalid_edit_recovery_semantic_no_progress_key".to_string(),
                "tool_allowed_false".to_string(),
                "tool_choice_auto".to_string(),
                "broad_surface_terminal_guard".to_string(),
                "rejected_tool_required_action_terminal_guard".to_string(),
                "lifecycle_kernel_provider_noncompliance".to_string(),
                "provider_ignored_edit_only_surface".to_string(),
                "malformed_tool_arguments_terminal_guard".to_string(),
                "result_hash_evidence_not_key".to_string(),
                "repair_required_edit_before_verification_rerun".to_string(),
            ],
            forbidden_refs: vec![
                "full_argument_hash".to_string(),
                "projection_id".to_string(),
                "required_singleton_only".to_string(),
                "required_verification_commands_after_repair".to_string(),
                "then_rerun_verification_as_current_action".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "HistoryItem ActiveWorkContract::Verification repair_required=true TurnControlEnvelope ActionAuthority target_scoped_read_grounding repair_required_edit_surface".to_string(),
            required_refs: vec![
                "ActiveWorkContract".to_string(),
                "repair_required=true".to_string(),
                "ActionAuthority".to_string(),
                "target_scoped_read_grounding".to_string(),
                "repair_required_edit_surface".to_string(),
            ],
            forbidden_refs: vec![
                "continuation_shell_override".to_string(),
                "prompt_candidate_surface_authority".to_string(),
                "duplicate_success_cache".to_string(),
                "read_disallowed_before_source_repair_grounding".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.executed_failure_call_output_terminal_guard"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolLifecycleEnvelope FunctionCallOutput success=false executed_tool_failure result_hash allowed_surface terminal_guard".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "executed_tool_failure".to_string(),
                "result_hash".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "assistant_error_only_tool_failure".to_string(),
                "outer_timeout_on_repeated_io_error".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.synthetic_feedback_not_verification_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolLifecycleEnvelope synthetic_corrective_feedback no_verification_run_result preserve_previous_VerificationFailureCluster no_summary_parser_authority".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "synthetic_corrective_feedback".to_string(),
                "no_verification_run_result".to_string(),
                "preserve_previous_VerificationFailureCluster".to_string(),
            ],
            forbidden_refs: vec![
                "synthetic_feedback_as_verification_run".to_string(),
                "raw_summary_parser_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.workspace_relative_file_change_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "FileChangeEvidence workspace_relative_path workspace_path_separator_normalization workspace_prefix_boundary session_cwd_authority no_route_root_relative_closeout_target GlobTool workspace_relative_pattern_match model_visible_relative_output".to_string(),
            required_refs: vec![
                "FileChangeEvidence".to_string(),
                "workspace_relative_path".to_string(),
                "workspace_path_separator_normalization".to_string(),
                "workspace_prefix_boundary".to_string(),
                "session_cwd_authority".to_string(),
                "GlobTool".to_string(),
                "workspace_relative_pattern_match".to_string(),
                "model_visible_relative_output".to_string(),
            ],
            forbidden_refs: vec![
                "route_root_path_as_required_read".to_string(),
                "nested_workspace_read_target".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.shell_mutation_syncs_edit_baseline"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ShellTool detected_file_changes absolute_paths EditSafety confirmed_content_baseline write_apply_patch_stale_guard".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "detected_file_changes".to_string(),
                "absolute_paths".to_string(),
                "EditSafety".to_string(),
                "confirmed_content_baseline".to_string(),
                "write_apply_patch_stale_guard".to_string(),
            ],
            forbidden_refs: vec![
                "shell_change_relative_path_invalidation".to_string(),
                "shell_file_change_without_edit_baseline".to_string(),
                "stale_guard_requires_read_when_read_disallowed".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.shell_output_encoding_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ShellTool stdout_bytes stderr_bytes display_projection Windows CP932 Shift_JIS fallback PYTHONUTF8 PYTHONIOENCODING".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "stdout_bytes".to_string(),
                "stderr_bytes".to_string(),
                "display_projection".to_string(),
                "PYTHONUTF8".to_string(),
                "PYTHONIOENCODING".to_string(),
            ],
            forbidden_refs: vec![
                "unconditional_utf8_lossy_decoding".to_string(),
                "japanese_mojibake_in_shell_output".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.command_text_encoding_contract"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CommandTextEncodingReview command_text_encoding_contract text_io_surface encoding_explicit encoding_inherited_from_tool_environment encoding_unspecified powershell_get_content_utf8_explicit command_correction ToolResult corrective_result".to_string(),
            required_refs: vec![
                "command_text_encoding_contract".to_string(),
                "encoding_explicit".to_string(),
                "encoding_inherited_from_tool_environment".to_string(),
                "encoding_unspecified".to_string(),
                "powershell_get_content_utf8_explicit".to_string(),
                "text_io_surface".to_string(),
                "corrective_result".to_string(),
            ],
            forbidden_refs: vec![
                "python_unittest_primary_key".to_string(),
                "hidden_utf8_bootstrap_as_readiness_evidence".to_string(),
                "platform_default_text_io".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.shell_timeout_process_tree_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ShellTool timeout cancellation normal_completion descendant_process_tree_first parent_shell_kill_after_tree completion_descendant_cleanup_before_pipe_join bounded_wait no_orphan_child_process".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "timeout".to_string(),
                "cancellation".to_string(),
                "descendant_process_tree_first".to_string(),
                "parent_shell_kill_after_tree".to_string(),
                "completion_descendant_cleanup_before_pipe_join".to_string(),
                "bounded_wait".to_string(),
            ],
            forbidden_refs: vec![
                "parent_shell_killed_before_descendants".to_string(),
                "orphan_child_process_after_timeout".to_string(),
                "orphan_child_process_after_parent_exit".to_string(),
                "pending_tool_lifecycle_after_timeout".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.closed_network_shell_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ShellTool external_connection_review environment_setup_review mandatory_user_confirmation shell_output_projection stdout stderr exit_code retry_guidance".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "external_connection_review".to_string(),
                "environment_setup_review".to_string(),
                "shell_output_projection".to_string(),
                "stdout".to_string(),
                "stderr".to_string(),
                "exit_code".to_string(),
                "retry_guidance".to_string(),
            ],
            forbidden_refs: vec![
                "agent_executed_external_connection_without_review".to_string(),
                "agent_executed_environment_setup_without_review".to_string(),
                "shell_output_hidden_from_desktop_history".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.vision.input_item_lifecycle_authority".to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "CodexUserInput LocalImage ContentItem::InputImage image_label provider_visible_image_item diagnostic_source_path_not_workspace_authority".to_string(),
            required_refs: vec![
                "CodexUserInput".to_string(),
                "LocalImage".to_string(),
                "ContentItem::InputImage".to_string(),
                "image_label".to_string(),
                "provider_visible_image_item".to_string(),
            ],
            forbidden_refs: vec![
                "source_path_as_workspace_file_authority".to_string(),
                "filename_required_for_image_discovery".to_string(),
                "glob_required_to_find_attached_image".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.workspace.absolute_turn_cwd_root_authority".to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexTurnContext absolute_cwd absolute_workspace_root fixed_harness_workspace_root non_empty_root shell_execution_context".to_string(),
            required_refs: vec![
                "CodexTurnContext".to_string(),
                "absolute_cwd".to_string(),
                "absolute_workspace_root".to_string(),
                "fixed_harness_workspace_root".to_string(),
                "shell_execution_context".to_string(),
            ],
            forbidden_refs: vec![
                "empty_workspace_root".to_string(),
                "relative_turn_cwd".to_string(),
                "enclosing_repo_root_for_case_workspace".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.lifecycle_kernel.provider_noncompliance_adjudication"
                .to_string(),
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            authority_source: "TurnLifecycleKernel ProviderActionAdapter ActionAdjudicator ModelActionProposal ProviderNoncompliance RejectedModelAction malformed_tool_arguments text_final_while_obligations_open rejected_tool_proposal tool_feedback_envelope no_progress_result_hash TurnControlEnvelope allowed_surface_snapshot".to_string(),
            required_refs: vec![
                "TurnLifecycleKernel".to_string(),
                "ProviderActionAdapter".to_string(),
                "ActionAdjudicator".to_string(),
                "ModelActionProposal".to_string(),
                "ProviderNoncompliance".to_string(),
                "RejectedModelAction".to_string(),
                "malformed_tool_arguments".to_string(),
                "text_final_while_obligations_open".to_string(),
                "rejected_tool_proposal".to_string(),
                "tool_feedback_envelope".to_string(),
                "no_progress_result_hash".to_string(),
                "TurnControlEnvelope".to_string(),
                "allowed_surface_snapshot".to_string(),
            ],
            forbidden_refs: vec![
                "manual_st_case_primary_key".to_string(),
                "calculator_primary_key".to_string(),
                "provider_wording_only".to_string(),
                "loop_local_shell_guard".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.lifecycle_kernel.turn_lifecycle_plan_authority".to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "TurnLifecycleKernel TurnLifecyclePlan stable_surface_default open_obligation_final_message_recovery failed_edit_final_message_recovery provider_noncompliance_edit_recovery provider_noncompliance_recovery_overrides_grounding wrong_target_authoring_edit_recovery wrong_target_generated_test_source_reference_read provider_required_tool_choice_final_message_recovery docs_provider_required_final_message_required_tool_choice code_authoring_final_message_hard_edit_recovery tool_choice_auto hard_recovery_required replay_policy proposal_policy corrective_policy terminal_policy continuation_expectation diagnostics_projection".to_string(),
            required_refs: vec![
                "TurnLifecycleKernel".to_string(),
                "TurnLifecyclePlan".to_string(),
                "stable_surface_default".to_string(),
                "open_obligation_final_message_recovery".to_string(),
                "failed_edit_final_message_recovery".to_string(),
                "provider_noncompliance_edit_recovery".to_string(),
                "provider_noncompliance_recovery_overrides_grounding".to_string(),
                "wrong_target_authoring_edit_recovery".to_string(),
                "wrong_target_generated_test_source_reference_read".to_string(),
                "provider_required_tool_choice_final_message_recovery".to_string(),
                "docs_provider_required_final_message_required_tool_choice".to_string(),
                "code_authoring_final_message_hard_edit_recovery".to_string(),
                "tool_choice_auto".to_string(),
                "hard_recovery_required".to_string(),
                "replay_policy".to_string(),
                "proposal_policy".to_string(),
                "corrective_policy".to_string(),
                "terminal_policy".to_string(),
                "continuation_expectation".to_string(),
                "diagnostics_projection".to_string(),
            ],
            forbidden_refs: vec!["TurnRuntimeToolChoicePolicy".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.turn_decision.codex_stable_tool_surface_authority"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "CodexResponsesRequest ActiveWorkContract RequestedWorkAuthoring candidate_tool_surface ActionAuthority workspace_target_identity_normalization stable_tool_schema tool_choice_auto requested_work_singleton_stable_surface singleton_missing_target_apply_patch_action_auto_choice codex_style_code_authoring_omits_whole_file_write codex_style_code_authoring_omits_json_discovery_surface codex_style_docs_authoring_omits_non_codex_json_surface generated_test_source_reference_grounding_after_source_change singleton_missing_target_source_reference_or_create_authority generated_test_consumed_source_reference_active_target_grounding repair_target_aliases_collapse_to_singleton_write_action typed_required_action_rendered_text open_work_lifecycle_evidence normal_authoring_final_message_recovery_stable_surface failed_edit_final_message_recovery_keeps_failed_edit_surface docs_open_obligation_required_edit_recovery open_obligation_final_message_recovery_persists_across_no_progress_tool authoring_final_message_target_grounding_read docs_patch_context_final_message_grounding docs_existing_target_update_exact_read_grounding source_repair_exact_write_final_message_recovery hard_repair_recovery_executable_schema_surface harness_closeout_guard open_obligation_final_message_guard open_obligation_final_message_guard_context_key".to_string(),
            required_refs: vec![
                "CodexResponsesRequest".to_string(),
                "ActiveWorkContract".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "ActionAuthority".to_string(),
                "workspace_target_identity_normalization".to_string(),
                "stable_tool_schema".to_string(),
                "tool_choice_auto".to_string(),
                "requested_work_singleton_stable_surface".to_string(),
                "singleton_missing_target_apply_patch_action_auto_choice".to_string(),
                "codex_style_code_authoring_omits_whole_file_write".to_string(),
                "codex_style_code_authoring_omits_json_discovery_surface".to_string(),
                "codex_style_docs_authoring_omits_non_codex_json_surface".to_string(),
                "generated_test_source_reference_grounding_after_source_change".to_string(),
                "singleton_missing_target_source_reference_or_create_authority".to_string(),
                "generated_test_consumed_source_reference_active_target_grounding".to_string(),
                "repair_target_aliases_collapse_to_singleton_write_action".to_string(),
                "typed_required_action_rendered_text".to_string(),
                "open_work_lifecycle_evidence".to_string(),
                "normal_authoring_final_message_recovery_stable_surface".to_string(),
                "failed_edit_final_message_recovery_keeps_failed_edit_surface".to_string(),
                "docs_open_obligation_required_edit_recovery".to_string(),
                "open_obligation_final_message_recovery_persists_across_no_progress_tool"
                    .to_string(),
                "authoring_final_message_target_grounding_read".to_string(),
                "docs_patch_context_final_message_grounding".to_string(),
                "docs_existing_target_update_exact_read_grounding".to_string(),
                "source_repair_exact_write_final_message_recovery".to_string(),
                "hard_repair_recovery_executable_schema_surface".to_string(),
                "harness_closeout_guard".to_string(),
                "open_obligation_final_message_guard".to_string(),
                "open_obligation_final_message_guard_context_key".to_string(),
            ],
            forbidden_refs: vec![
                "required_action_string_grammar".to_string(),
                "schema_const_payload_injection".to_string(),
                "prompt_prose_only_write_authority".to_string(),
                "tool_choice_required_for_open_executable_work".to_string(),
                "named_write_with_broad_supporting_context_surface".to_string(),
                "whole_file_write_provider_surface".to_string(),
                "json_discovery_provider_surface".to_string(),
                "singleton_edit_only_null_required_action".to_string(),
                "source_repair_final_message_recovery_shell_projection".to_string(),
                "text_only_final_clean_closeout_with_open_obligations".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.turn_decision.active_work_edit_before_verification_rerun"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "CodexHistoryItemStream VerificationFailureCluster ActiveWorkContract Verification repair_required_active_work SourceViolatesContract SourceTestContractMismatch TestViolatesContract source_owned_requirement_refs_align_active_work_with_repair_lane contract_visible_public_exception_owner_authority generated_test_parse_defect_owner_authority generated_test_reflection_api_misuse_owner_authority generated_test_module_attribute_api_misuse_owner_authority generated_test_exception_type_overreach_owner_authority source_parse_defect_owner_authority no_tests_ran_recent_generated_test_target_authority generated_test_subprocess_encoding_owner_authority generated_test_subprocess_output_capture_owner_authority generated_test_name_resolution_owner_authority generated_test_import_nameerror_owner_authority mixed_source_test_contract_reconciliation_owner_authority generated_test_contract_overreach_owner_projection_alignment generic_generated_test_only_owner_target_authority ungrounded_generated_public_output_assertion_owner_authority generated_test_local_binding_contradiction_owner_authority deferred_verification_command_not_progress_evidence failed_patch_context_mismatch_target_grounding patch_context_mismatch_recovery_augments_read_surface ActionAuthority repair_lane_diagnostic_only stable_tool_schema call_id_scoped_outputs".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "VerificationFailureCluster".to_string(),
                "ActiveWorkContract".to_string(),
                "Verification".to_string(),
                "repair_required_active_work".to_string(),
                "SourceViolatesContract".to_string(),
                "SourceTestContractMismatch".to_string(),
                "TestViolatesContract".to_string(),
                "source_owned_requirement_refs_align_active_work_with_repair_lane"
                    .to_string(),
                "contract_visible_public_exception_owner_authority".to_string(),
                "generated_test_parse_defect_owner_authority".to_string(),
                "generated_test_reflection_api_misuse_owner_authority".to_string(),
                "generated_test_module_attribute_api_misuse_owner_authority".to_string(),
                "generated_test_exception_type_overreach_owner_authority".to_string(),
                "source_parse_defect_owner_authority".to_string(),
                "no_tests_ran_recent_generated_test_target_authority".to_string(),
                "generated_test_subprocess_encoding_owner_authority".to_string(),
                "generated_test_subprocess_output_capture_owner_authority".to_string(),
                "generated_test_contract_overreach_owner_projection_alignment".to_string(),
                "generic_generated_test_only_owner_target_authority".to_string(),
                "ungrounded_generated_public_output_assertion_owner_authority".to_string(),
                "generated_test_name_resolution_owner_authority".to_string(),
                "generated_test_import_nameerror_owner_authority".to_string(),
                "mixed_source_test_contract_reconciliation_owner_authority".to_string(),
                "generated_test_local_binding_contradiction_owner_authority".to_string(),
                "deferred_verification_command_not_progress_evidence".to_string(),
                "failed_patch_context_mismatch_target_grounding".to_string(),
                "patch_context_mismatch_recovery_augments_read_surface".to_string(),
                "ActionAuthority".to_string(),
                "repair_lane_diagnostic_only".to_string(),
                "stable_tool_schema".to_string(),
                "call_id_scoped_outputs".to_string(),
            ],
            forbidden_refs: vec![
                "stale_shell_rerun_before_repair_edit".to_string(),
                "repair_lane_top_level_override".to_string(),
                "prompt_surface_only_tool_authority".to_string(),
                "exact_recorded_verification_command_rerun_as_progress".to_string(),
                "no_tests_ran_targetless_repair_unclassified".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.final_assistant_message_lifecycle".to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "CodexTurnComplete final_assistant_message no_synthetic_completion_tool no_closeout_reread_tool no_open_obligations answer_only_no_executable_work bounded_closeout_final_response_timeout satisfied_item_stream_terminal_guard".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "final_assistant_message".to_string(),
                "no_synthetic_completion_tool".to_string(),
                "no_open_obligations".to_string(),
                "answer_only_no_executable_work".to_string(),
                "bounded_closeout_final_response_timeout".to_string(),
                "satisfied_item_stream_terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "synthetic_completion_required_action".to_string(),
                "mandatory_closeout_reread".to_string(),
                "unbounded_closeout_provider_wait".to_string(),
                "answer_only_final_message_rejected_by_closeout_flag".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.open_obligation_final_assistant_continuation_hook"
                .to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "CodexTurnComplete StopRequest hook_prompt_message text_only_hook_prompt RuntimeCompleted RuntimeDidNotComplete final_assistant_message OpenObligation ManualStCloseoutEvidence CloseoutContinuationUserTurn missing_artifacts file_changing_tool_call_required expected_artifacts_inventory_non_authoring current_workspace_artifact_clears_stale_authoring_obligation satisfied_docs_repair_not_open_closeout route_verification_waits_for_artifact_authoring latest_verification_command_evidence current_run_error_closeout_projection runtime_error_open_obligation_continuation_budget runtime_terminal_status_open_obligation_continuation_budget same_workspace_continuation_budget successful_continuation_case_verdict_materialization route_terminal_verdict_case_result_materialization open_obligation_final_message_surface_insensitive_guard bounded_route_failure".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "StopRequest".to_string(),
                "hook_prompt_message".to_string(),
                "text_only_hook_prompt".to_string(),
                "RuntimeCompleted".to_string(),
                "RuntimeDidNotComplete".to_string(),
                "final_assistant_message".to_string(),
                "OpenObligation".to_string(),
                "ManualStCloseoutEvidence".to_string(),
                "CloseoutContinuationUserTurn".to_string(),
                "missing_artifacts".to_string(),
                "file_changing_tool_call_required".to_string(),
                "expected_artifacts_inventory_non_authoring".to_string(),
                "current_workspace_artifact_clears_stale_authoring_obligation".to_string(),
                "satisfied_docs_repair_not_open_closeout".to_string(),
                "route_verification_waits_for_artifact_authoring".to_string(),
                "latest_verification_command_evidence".to_string(),
                "current_run_error_closeout_projection".to_string(),
                "runtime_error_open_obligation_continuation_budget".to_string(),
                "runtime_terminal_status_open_obligation_continuation_budget".to_string(),
                "same_workspace_continuation_budget".to_string(),
                "successful_continuation_case_verdict_materialization".to_string(),
                "route_terminal_verdict_case_result_materialization".to_string(),
                "open_obligation_final_message_surface_insensitive_guard".to_string(),
                "bounded_route_failure".to_string(),
            ],
            forbidden_refs: vec![
                "hidden_retry".to_string(),
                "assistant_error_retry".to_string(),
                "reattach_original_images".to_string(),
                "same_message_retry".to_string(),
                "tool_choice_required_for_open_executable_work".to_string(),
                "session_completed_is_clean_closeout".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.verification_failure_preserves_closeout_evidence"
                .to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "CodexTurnComplete RuntimeCompleted final_assistant_message OpenObligation ManualStCloseoutEvidence verification_failed missing_artifacts route_verification_supporting_evidence".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "RuntimeCompleted".to_string(),
                "final_assistant_message".to_string(),
                "OpenObligation".to_string(),
                "ManualStCloseoutEvidence".to_string(),
                "verification_failed".to_string(),
                "missing_artifacts".to_string(),
                "route_verification_supporting_evidence".to_string(),
            ],
            forbidden_refs: vec![
                "verification_failure_short_circuit".to_string(),
                "verification_failure_replaces_closeout_evidence".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.verification_repair_continuation_hook".to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "CodexTurnComplete StopRequest hook_prompt_message text_only_hook_prompt RuntimeCompleted final_assistant_message VerificationFailed latest_failed_command verification_failure_evidence repair_target write_or_apply_patch_required rerun_failed_command failure_signature_scoped_budget no_image_reattach".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "StopRequest".to_string(),
                "hook_prompt_message".to_string(),
                "text_only_hook_prompt".to_string(),
                "RuntimeCompleted".to_string(),
                "VerificationFailed".to_string(),
                "latest_failed_command".to_string(),
                "verification_failure_evidence".to_string(),
                "repair_target".to_string(),
                "write_or_apply_patch_required".to_string(),
                "rerun_failed_command".to_string(),
                "failure_signature_scoped_budget".to_string(),
            ],
            forbidden_refs: vec![
                "stage_scoped_flat_budget".to_string(),
                "missing_artifact_prompt_for_verification_failure".to_string(),
                "assistant_error_retry".to_string(),
                "hidden_retry".to_string(),
                "reattach_original_images".to_string(),
                "tool_choice_required_for_open_executable_work".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.verification_labels_not_requested_work".to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "CodexHistoryItemStream FunctionCallOutput VerificationFailed evidence_labels diagnostic_traceback_paths continuation_context_sections requested_work_authoring_targets deliverable_artifact_paths ManualStCloseoutEvidence verification_repair_hook stable_tool_schema no_tool_closeout_prevention".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "FunctionCallOutput".to_string(),
                "VerificationFailed".to_string(),
                "evidence_labels".to_string(),
                "diagnostic_traceback_paths".to_string(),
                "continuation_context_sections".to_string(),
                "requested_work_authoring_targets".to_string(),
                "deliverable_artifact_paths".to_string(),
                "ManualStCloseoutEvidence".to_string(),
                "verification_repair_hook".to_string(),
                "stable_tool_schema".to_string(),
            ],
            forbidden_refs: vec![
                "test_method_symbol_as_deliverable".to_string(),
                "verification_label_requested_work_target".to_string(),
                "traceback_path_requested_work_target".to_string(),
                "previous_final_message_symbol_requested_work_target".to_string(),
                "no_tool_closeout_with_content_changing_authoring_required".to_string(),
                "missing_artifact_prompt_for_verification_failure".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.verification.typed_evidence_cluster_authority".to_string(),
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            authority_source: "VerificationRunResult VerificationFailureCluster VerificationFailureEvidence requirement_refs artifact_refs evidence_markers".to_string(),
            required_refs: vec![
                "VerificationFailureCluster".to_string(),
                "VerificationFailureEvidence".to_string(),
                "requirement_refs".to_string(),
            ],
            forbidden_refs: vec!["failure_summary_contains_authority".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.desktop_transcript.completed_primary_reading_path".to_string(),
            family: PreflightGateFamily::DesktopTranscriptProjectionAuthority,
            authority_source: "DesktopTranscriptProjection canonical_turn_item_stream chronological_turn_blocks primary_reading_path work_summary_completed collapsed_work_history final_assistant_closeout typed_terminal_outcome_authority typed_file_change_rows intermediate_assistant_folded control_feedback_folded tool_feedback_folded".to_string(),
            required_refs: vec![
                "DesktopTranscriptProjection".to_string(),
                "canonical_turn_item_stream".to_string(),
                "chronological_turn_blocks".to_string(),
                "primary_reading_path".to_string(),
                "work_summary_completed".to_string(),
                "collapsed_work_history".to_string(),
                "final_assistant_closeout".to_string(),
                "typed_terminal_outcome_authority".to_string(),
                "typed_file_change_rows".to_string(),
                "intermediate_assistant_folded".to_string(),
                "control_feedback_folded".to_string(),
                "tool_feedback_folded".to_string(),
            ],
            forbidden_refs: vec![
                "case1_primary_key".to_string(),
                "latest_user_primary_key".to_string(),
                "raw_tool_log_primary_path".to_string(),
                "intermediate_assistant_as_primary_response".to_string(),
                "cancelled_assistant_intent_as_final_outcome".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.manual_st.route_evidence_schema".to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "route_manifest case_progress verification_command_log workspace_diff_manifest request_payload_summary timeout_classification active_case_progress_status inflight_case_session_progress case_progress_phase_boundaries prompt_visible_scenario_contract_authority stage_scoped_verification_spec_authority explicit_session_continuation no_continue_last_with_session provider_stream_idle_timeout_classification provider_stream_retry_exhausted_classification provider_transport_stream_error_classification semantic_no_progress_terminal_classification route_owned_command_timeout route_command_stdin_closed".to_string(),
            required_refs: vec![
                "route_manifest".to_string(),
                "case_progress".to_string(),
                "verification_command_log".to_string(),
                "workspace_diff_manifest".to_string(),
                "active_case_progress_status".to_string(),
                "inflight_case_session_progress".to_string(),
                "case_progress_phase_boundaries".to_string(),
                "prompt_visible_scenario_contract_authority".to_string(),
                "stage_scoped_verification_spec_authority".to_string(),
                "explicit_session_continuation".to_string(),
                "no_continue_last_with_session".to_string(),
                "provider_stream_idle_timeout_classification".to_string(),
                "provider_stream_retry_exhausted_classification".to_string(),
                "provider_transport_stream_error_classification".to_string(),
                "semantic_no_progress_terminal_classification".to_string(),
                "route_owned_command_timeout".to_string(),
            ],
            forbidden_refs: vec!["case_primary_key".to_string()],
            required_artifacts: required_manual_st_artifacts(),
            fail_closed_on_missing_typed_projection: false,
        },
    ]
}

pub fn run_default_active_preflight() -> PreflightReport {
    PreflightRunner::run_active(
        &failure_registry_preflight_suite(),
        &default_preflight_fixtures(),
    )
}

pub fn run_artifact_replay_preflight(
    artifact_root: &Utf8Path,
    failure_ids: Vec<String>,
) -> Result<PreflightReport, RuntimeError> {
    if !artifact_root.exists() {
        return Err(RuntimeError::Message(format!(
            "artifact root `{artifact_root}` does not exist"
        )));
    }

    let required_artifacts = required_manual_st_artifacts();
    let missing = required_artifacts
        .iter()
        .filter(|artifact| !artifact_root.join(artifact.as_str()).exists())
        .cloned()
        .collect::<Vec<_>>();

    let mut diagnostics = Vec::new();
    if !failure_ids.is_empty() {
        diagnostics.push(format!(
            "failure evidence ids are replay metadata only: {}",
            failure_ids.join(",")
        ));
    }
    if missing.is_empty() {
        diagnostics.push("artifact root satisfies Codex-style route evidence schema".to_string());
    } else {
        diagnostics.push(format!(
            "artifact root is missing required route evidence artifacts: {}",
            missing.join(", ")
        ));
    }

    Ok(PreflightReport::from_results(vec![PreflightGateReport {
        gate_id: "preflight.artifact.route_evidence_schema".to_string(),
        fixture_id: Some("fixture.artifact.route_evidence_schema".to_string()),
        layer: PreflightLayer::HarnessReplay,
        family: Some(PreflightGateFamily::ArtifactReplaySchema),
        status: if missing.is_empty() {
            PreflightResultStatus::Pass
        } else {
            PreflightResultStatus::Fail
        },
        diagnostics,
        evidence_refs: required_artifacts,
    }]))
}

pub fn write_preflight_report(
    report: &PreflightReport,
    output: &Utf8Path,
) -> Result<(), RuntimeError> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            RuntimeError::Message(format!(
                "failed to create preflight report dir `{parent}`: {error}"
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| {
        RuntimeError::Message(format!("failed to serialize preflight report: {error}"))
    })?;
    fs::write(output, bytes).map_err(|error| {
        RuntimeError::Message(format!(
            "failed to write preflight report `{output}`: {error}"
        ))
    })
}

fn required_manual_st_artifacts() -> Vec<String> {
    vec![
        "route_manifest.json".to_string(),
        "case_progress.json".to_string(),
        "verification_command_log.json".to_string(),
        "workspace_diff_manifest.json".to_string(),
        "result.json".to_string(),
        "preflight_report.json".to_string(),
        "timeout_classification.json".to_string(),
    ]
}

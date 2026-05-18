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

    if gate.gate_id == "preflight.item_lifecycle.provider_replay_call_output_symmetry"
        && !provider_replay_call_output_symmetry_fixture_passes()
    {
        diagnostics.push(
            "provider replay is not built from canonical HistoryItem call/output pairs, or orphan/error items can still become assistant text".to_string(),
        );
    }

    if gate.gate_id == "preflight.llm_transport.stream_retry_before_first_event"
        && !crate::llm::openai_compat::stream_event_retry_classifier_fixture_passes()
    {
        diagnostics.push(
            "provider SSE decode/transport failures before the first emitted model event are not classified as retryable, or non-transport stream errors are retryable"
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

    if matches!(gate.family, PreflightGateFamily::StateReducerAuthority)
        && !state_reducer_runtime_feedback_fixture_passes()
    {
        diagnostics.push(
            "recoverable runtime feedback was classified as verification repair authority"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.state_reducer.requested_work_completion_promotes_verification"
        && (!crate::agent::state::requested_work_completion_promotes_verification_fixture_passes()
            || !crate::agent::state::required_verification_survives_authoring_completion_fixture_passes()
            || !crate::agent::state::partial_requested_work_remains_authoring_phase_fixture_passes(
            )
            || !crate::agent::state::passed_verification_consumes_pending_required_commands_fixture_passes()
            || !crate::agent::state::resumed_new_user_turn_ignores_prior_closeout_fixture_passes()
            || !crate::agent::state::new_authoring_turn_overrides_prior_verification_fixture_passes()
            || !crate::agent::state::partial_verification_pass_preserves_remaining_required_commands_fixture_passes()
            || !crate::agent::state::reference_design_input_does_not_become_pending_authoring_target_fixture_passes()
            || !crate::agent::state::same_document_reference_update_remains_authoring_target_fixture_passes()
            || !crate::agent::state::japanese_prompt_filename_boundaries_remain_artifact_targets_fixture_passes()
            || !crate::agent::state::docs_output_referenced_code_does_not_become_pending_authoring_target_fixture_passes()
            || !crate::agent::state::requested_work_without_verification_closes_after_file_change_fixture_passes()
            || !crate::agent::state::structured_document_summary_waits_for_remaining_sources_fixture_passes()
            || !crate::agent::state::structured_document_summary_output_headings_survive_compacted_history_fixture_passes())
    {
        diagnostics.push(
            "requested-work item-stream evidence did not preserve partial authoring phase, keep reference inputs out of unrelated pending deliverables, preserve same-document reference updates as authoring targets, keep staged structured-document summaries open until all sources are processed, recover structured-document progress from output headings after compacted history, close no-verification deliverables after FileChange evidence, promote completed authoring to exact verification shell authority before closeout, retain explicit verification commands after latest authoring writes, keep resumed/new authoring turns out of stale closeout or verification authority, or consume passed verification commands before clean closeout".to_string(),
        );
    }

    if gate.gate_id == "preflight.state_reducer.post_repair_edit_promotes_verification_rerun"
        && (!crate::agent::state::post_repair_file_change_promotes_verification_rerun_fixture_passes()
            || !crate::agent::turn_decision::post_repair_edit_progress_promotes_shell_rerun_fixture_passes())
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

    if matches!(gate.family, PreflightGateFamily::PromptReplayAuthority)
        && !prompt_replay_stale_write_fixture_passes()
    {
        diagnostics.push(
            "stale write payload would remain provider-visible as current action authority"
                .to_string(),
        );
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

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::agent::prompt::stale_inactive_authoring_replay_omits_fake_executable_arguments()
    {
        diagnostics.push(
            "provider replay can still expose stale inactive authoring sentinel values as executable assistant tool-call arguments".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.progress_projection_pair_omitted"
        && !crate::agent::prompt::provider_replay_omits_stale_progress_projection_arguments()
    {
        diagnostics.push(
            "provider replay can still expose stale progress-projection todo JSON as executable assistant tool-call arguments or omit current call-id-scoped progress feedback".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.no_content_write_is_no_progress"
        && (!crate::agent::tool_orchestrator::no_content_write_metadata_projects_no_progress_fixture_passes()
            || !crate::tool::apply_patch::destructive_noop_patch_is_rejected_fixture_passes()
            || !crate::tool::apply_patch::empty_or_zero_diff_patch_is_rejected_fixture_passes()
            || !crate::tool::apply_patch::hunkless_update_patch_is_rejected_fixture_passes())
    {
        diagnostics.push(
            "no-content write output or destructive no-op acknowledgement patch can still be projected as successful repair progress".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.active_authoring_rejects_wrong_target"
        && !crate::agent::loop_impl::active_authoring_rejects_wrong_target_fixture_passes()
    {
        diagnostics.push(
            "requested-work authoring still accepts content-changing writes outside the active deliverable set as progress".to_string(),
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
            || !crate::agent::loop_impl::executed_tool_failure_terminal_guard_fixture_passes())
    {
        diagnostics.push(
            "executed tool failures are not preserved as call-scoped failed outputs with stable no-progress terminal guard".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.verification_stable_tool_surface"
        && (!crate::agent::loop_impl::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes()
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

    if gate.gate_id == "preflight.tool_lifecycle.progress_projection_stable_surface_guard"
        && !crate::agent::loop_impl::progress_projection_stable_surface_guard_fixture_passes()
    {
        diagnostics.push(
            "requested-work authoring still hides todowrite from provider-visible schema or fails to guard progress projection as call-scoped no-progress evidence".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.edit_surface_registry_symmetry"
        && !crate::agent::loop_impl::edit_surface_registry_symmetry_fixture_passes()
    {
        diagnostics.push(
            "core edit tool surface and runtime dispatch registry can still diverge, or failed inactive write feedback is not preserved as a call-id-scoped ToolCall/ToolOutput pair".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"
        && !crate::agent::loop_impl::rejected_tool_semantic_terminal_guard_fixture_passes()
    {
        diagnostics.push(
            "rejected known-tool feedback still uses unstable argument/projection keys or fails to terminalize repeated disallowed proposals before outer timeout".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.workspace_relative_file_change_authority"
        && !crate::edit::change_path_storage_uses_workspace_relative_authority()
    {
        diagnostics.push(
            "file-change lifecycle still stores route-root or absolute paths instead of workspace-relative authority".to_string(),
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

    if gate.gate_id == "preflight.vision.input_item_lifecycle_authority"
        && (!crate::agent::prompt::vision_input_provider_projection_fixture_passes()
            || !crate::harness::manual_st::vision_prompt_uses_labeled_attachment_fixture_passes())
    {
        diagnostics.push(
            "vision input items are not projected as Codex-style labeled image content, or diagnostic source paths still leak into provider-visible workspace authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.workspace.absolute_turn_cwd_root_authority"
        && !crate::workspace::discovery::workspace_discovery_absolute_root_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "workspace discovery can still produce an empty or relative turn cwd/root authority"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.turn_decision.codex_stable_tool_surface_authority"
        && (!crate::agent::loop_impl::singleton_write_surface_requires_tool_choice_fixture_passes()
            || !crate::agent::loop_impl::concrete_write_required_action_narrows_broad_surface_fixture_passes()
            || !crate::agent::loop_impl::open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes()
            || !crate::agent::loop_impl::open_obligation_final_message_guard_fixture_passes())
    {
        diagnostics.push(
            "tool schemas or tool choice still derive from provider-visible required-action strings, open executable work still forces required tool_choice instead of Codex-style auto sampling plus lifecycle/harness evidence, or open obligations can still accept a text-only final assistant message as clean closeout".to_string(),
        );
    }

    if gate.gate_id == "preflight.turn_decision.active_work_edit_before_verification_rerun"
        && (!crate::agent::state::verification_failure_promotes_repair_required_active_work_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_stays_diagnostic_fixture_passes()
            || !crate::agent::turn_decision::active_work_edit_authority_precedes_verification_rerun_fixture_passes()
            || !crate::agent::loop_impl::required_repair_write_missing_tool_is_not_restored_fixture_passes()
            || !crate::agent::loop_impl::verification_repair_required_edit_surface_narrows_stale_tools_fixture_passes())
    {
        diagnostics.push(
            "source-owned verification repair is not compiled through item-stream active work / ActionAuthority, or repair-lane diagnostics still override the top-level dispatch authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.final_assistant_message_lifecycle"
        && (!crate::agent::loop_impl::clean_closeout_final_message_lifecycle_fixture_passes()
            || !crate::agent::loop_impl::answer_only_final_message_lifecycle_fixture_passes()
            || !crate::agent::loop_impl::closeout_ready_final_response_timeout_guard_fixture_passes(
            ))
    {
        diagnostics.push(
            "clean closeout still requires a synthetic completion tool, no-executable-work answer-only turns can still reject final assistant messages, or closeout can wait indefinitely for a provider final message after item-stream evidence is already satisfied".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.open_obligation_final_assistant_continuation_hook"
        && (!crate::harness::manual_st::final_assistant_open_obligation_not_clean_closeout_fixture_passes()
            || !crate::harness::manual_st::final_assistant_open_obligation_continuation_hook_fixture_passes()
            || !crate::harness::manual_st::closeout_continuation_is_text_only_fixture_passes()
            || !crate::harness::manual_st::latest_verification_result_drives_closeout_fixture_passes())
    {
        diagnostics.push(
            "runtime-completed final assistant messages with open obligations are not converted into explicit text-only continuation user-turn items or closeout verification does not use latest command evidence".to_string(),
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
            || !crate::harness::manual_st::closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes())
    {
        diagnostics.push(
            "failed verification closeout does not project a Codex-style verification-repair hook prompt, or closeout continuation budget is still scoped to the whole stage instead of the failure signature".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.verification_labels_not_requested_work"
        && (!crate::agent::state::verification_failure_labels_are_not_requested_work_targets_fixture_passes()
            || !crate::harness::manual_st::verification_failure_labels_do_not_become_authoring_obligations_fixture_passes())
    {
        diagnostics.push(
            "failed verification labels or test method symbols can still become requested-work authoring targets, or failed verification closeout can still lose its edit-capable repair hook lifecycle".to_string(),
        );
    }

    if gate.gate_id == "preflight.route_evidence.schema"
        && (!crate::harness::manual_st::multistage_continuation_uses_explicit_session_without_continue_last_fixture_passes()
            || !crate::harness::manual_st::provider_stream_idle_timeout_classification_fixture_passes()
            || !crate::harness::manual_st::provider_transport_stream_error_classification_fixture_passes()
            || !crate::harness::manual_st::semantic_no_progress_terminal_classification_fixture_passes()
            || !crate::harness::manual_st::route_evidence_filters_generated_dependency_paths_fixture_passes()
            || !crate::harness::manual_st::route_evidence_overwrites_stale_timeout_classification_fixture_passes())
    {
        diagnostics.push(
            "manual ST route evidence can still lose explicit session continuation, provider stream timeout classification, fresh output-root ownership, or bounded workspace manifest filtering".to_string(),
        );
    }

    if gate.gate_id
        == "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
        && (!crate::agent::state::verification_failure_preserves_repair_targets_fixture_passes()
            || !crate::agent::state::source_owned_verification_failure_preserves_recent_source_edit_target_fixture_passes()
            || !crate::agent::state::out_of_order_history_items_use_sequence_authority_for_repair_fixture_passes()
            || !crate::agent::state::verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes()
            || !crate::agent::state::verification_failure_ignores_runtime_loader_frame_fixture_passes()
            || !crate::agent::repair_lane::source_owned_verification_repair_lane_fixture_passes()
            || !crate::agent::repair_lane::source_owned_repair_lane_rejects_diagnostic_label_targets_fixture_passes()
            || !crate::agent::contract_reconciliation::contract_reconciliation_ignores_diagnostic_label_targets_fixture_passes())
    {
        diagnostics.push(
            "verification failure projection can still erase source/test repair targets or fail-closed before dispatch".to_string(),
        );
    }

    if gate.gate_id == "preflight.state_reducer.docs_route_contract_authority"
        && (!crate::agent::state::docs_route_contract_promotes_docs_repair_fixture_passes()
            || !crate::agent::state::docs_route_localized_topic_completion_fixture_passes()
            || !crate::agent::loop_impl::docs_route_semantic_no_progress_guard_fixture_passes()
            || !crate::agent::loop_impl::docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_survives_partial_write_fixture_passes()
            || !crate::agent::loop_impl::docs_route_rejects_completed_deliverable_regression_fixture_passes()
            || !crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes())
    {
        diagnostics.push(
            "docs-only route contract can still degrade to generic requested-work authoring, unbounded read-only churn, or prompt projection without a write-ready docs boundary".to_string(),
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

fn protocol_item_lifecycle_fixture_passes() -> bool {
    use crate::protocol::{
        HistoryItemPayload, ProtocolEventStore, ProtocolRecordingSink, SqliteProtocolEventStore,
        TurnId,
    };
    use crate::runtime::RunEventSink;
    use crate::session::{RunEvent, SessionId, ToolCallId};
    use crate::tool::ToolName;
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
    let pending_metadata = serde_json::json!({
        "tool_route": {
            "original_arguments": {"path": "wrong.py", "content": "old"},
            "effective_arguments": {"path": "right.py", "content": "new"},
            "adjusted_arguments": {"path": "right.py", "content": "new"},
            "allowed_tools": ["write"],
            "permission_decision": "not_required",
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
    has_effective_args && has_typed_success
}

fn state_reducer_runtime_feedback_fixture_passes() -> bool {
    crate::agent::state::runtime_feedback_summary_preserves_completion_authority(
        "The previous response did not use any tools while typed work remains: \
         active_work=author test_calculator.py. Runtime requires a call through one of the \
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

fn prompt_replay_stale_write_fixture_passes() -> bool {
    crate::agent::prompt::stale_write_tool_call_replay_is_summary_only(
        r#"{"path":"calculator.py","content":"previous source content"}"#,
        "test_calculator.py",
    ) && crate::agent::prompt::stale_write_tool_call_replay_omits_payload(
        r#"{"path":"calculator.py","content":"def calculate(): pass"}"#,
        "test_calculator.py",
        "def calculate",
    ) && crate::agent::prompt::stale_write_prelude_replay_omits_text(
        "test_calculator.py",
        "calculator.py",
    ) && crate::agent::prompt::stale_todo_progress_replay_omits_prior_plan(
        "test_calculator.py",
        "ディレクトリは空です。`calculator.py` と `test_calculator.py` を作成します。",
    ) && prompt_replay_internal_control_items_are_not_provider_visible()
        && write_schema_stays_provider_owned()
        && crate::agent::prompt::exact_write_target_contract_projects_content_authority(
            "test_calculator.py",
        )
        && crate::agent::loop_impl::required_write_target_mismatch_feedback_projects_test_content_authority()
        && crate::agent::loop_impl::concrete_write_required_action_narrows_broad_surface_fixture_passes()
        && crate::agent::loop_impl::exact_write_route_accepts_unittest_main_test_content()
        && crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden()
        && crate::agent::loop_impl::content_shape_mismatch_feedback_carries_positive_test_contract()
        && crate::agent::tool_result_classification::required_write_content_shape_mismatch_is_nonprogress()
        && crate::tool::apply_patch::destructive_noop_patch_is_rejected_fixture_passes()
        && crate::agent::prompt::content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload()
        && crate::agent::prompt::stale_inactive_authoring_replay_uses_live_builder()
        && crate::agent::prompt::exact_authoring_write_required_preserves_source_progress_projection(
        )
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
                    text: "create test_calculator.py".to_string(),
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
    provider_visible_text.contains("test_calculator.py")
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
                output_text: "calculator.py".to_string(),
                metadata: serde_json::json!({"success": true}),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                required_next_action: None,
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
                required_next_action: None,
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
                if replayed == &call_id.to_string() && result.contains("calculator.py")
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
            purpose: "canonical HistoryItem/TurnItem stream remains the runtime and harness authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ProtocolItemLifecycle,
            fixture_id: "fixture.protocol.history_item_lifecycle_authority".to_string(),
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
            purpose: "successful content-changing repair FileChangeEvidence satisfies the current repair target and promotes the next dispatch to exact verification rerun".to_string(),
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
            purpose: "failed verification outputs preserve prior obligation targets and typed source-owned repair authority before the next provider dispatch".to_string(),
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
            purpose: "docs-only long-context tasks materialize as TaskRoute::Docs with DocsRouteState and DocsRepair, then bound read-only churn through call-id-scoped corrective tool output and a write-ready docs lifecycle boundary before generic requested-work filename authoring can own the turn".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::StateReducerAuthority,
            fixture_id: "fixture.state_reducer.docs_route_contract_authority".to_string(),
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
            purpose: "provider replay preserves call-id-scoped assistant tool-call/tool-output symmetry and uses model_arguments when compatibility arguments are absent".to_string(),
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
            purpose: "repeated disallowed known-tool feedback uses semantic lifecycle keys and terminalizes before outer timeout".to_string(),
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
            authority_source: "canonical_history_item_stream turn_item_stream typed_tool_arguments typed_file_change_evidence typed_tool_output_success".to_string(),
            required_refs: vec![
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
            authority_source: "ProviderStreamRetry stream_max_retries sse_transport_error body_decode_error retry_before_first_model_event no_retry_after_partial_model_event no_retry_for_parse_or_provider_error".to_string(),
            required_refs: vec![
                "ProviderStreamRetry".to_string(),
                "stream_max_retries".to_string(),
                "sse_transport_error".to_string(),
                "body_decode_error".to_string(),
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
            forbidden_refs: vec!["active_work_required_next_action_fallback".to_string(), "required_action_string_grammar".to_string()],
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
            authority_source: "RequestedWorkAuthoring ReferenceInput reference_input_not_pending_deliverable same_document_reference_update_authoring_target japanese_prompt_filename_token_boundary docs_output_referenced_code_not_pending_deliverable structured_document_summary_remaining_sources_block_closeout structured_document_summary_output_heading_progress_after_compaction no_verification_requested_work_file_change_closeout FileChangeEvidence authoring_complete Verification verification_command_obligation before_closeout canonical_item_chronology turn_local_sequence_no latest_content_change_invalidates_prior_verification VerificationRunResult passed_verification_command_consumed clean_closeout".to_string(),
            required_refs: vec![
                "RequestedWorkAuthoring".to_string(),
                "ReferenceInput".to_string(),
                "reference_input_not_pending_deliverable".to_string(),
                "same_document_reference_update_authoring_target".to_string(),
                "japanese_prompt_filename_token_boundary".to_string(),
                "docs_output_referenced_code_not_pending_deliverable".to_string(),
                "structured_document_summary_remaining_sources_block_closeout".to_string(),
                "structured_document_summary_output_heading_progress_after_compaction".to_string(),
                "no_verification_requested_work_file_change_closeout".to_string(),
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
            authority_source: "RepairControlSnapshot FileChangeEvidence content_changing_repair_progress target_normalization Verification exact_shell_rerun before_repair_reissue".to_string(),
            required_refs: vec![
                "RepairControlSnapshot".to_string(),
                "FileChangeEvidence".to_string(),
                "content_changing_repair_progress".to_string(),
                "target_normalization".to_string(),
                "Verification".to_string(),
                "exact_shell_rerun".to_string(),
            ],
            forbidden_refs: vec![
                "stale_repair_target_after_successful_write".to_string(),
                "repair_lane_top_level_required_action_mismatch".to_string(),
                "repair_lane_top_level_target_mismatch".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id:
                "fixture.state_reducer.verification_failure_preserves_repair_target_authority"
                    .to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "CodexHistoryItemStream session_state_projection_not_sequence_floor VerificationRunResult VerificationFailureCluster active_obligation_targets source_owned_repair_control_snapshot source_owned_recent_file_change_target_preserved verification_labels_not_file_targets python_runtime_traceback_frames_excluded import_error_module_target_authority no_fail_closed_dispatch".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "session_state_projection_not_sequence_floor".to_string(),
                "VerificationRunResult".to_string(),
                "VerificationFailureCluster".to_string(),
                "active_obligation_targets".to_string(),
                "source_owned_repair_control_snapshot".to_string(),
                "source_owned_recent_file_change_target_preserved".to_string(),
                "verification_labels_not_file_targets".to_string(),
                "python_runtime_traceback_frames_excluded".to_string(),
                "import_error_module_target_authority".to_string(),
            ],
            forbidden_refs: vec![
                "session_state_sequence_floor".to_string(),
                "empty_active_targets_after_verification_failure".to_string(),
                "repair_unclassified_before_dispatch".to_string(),
                "stdlib_unittest_loader_frame_as_repair_target".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.docs_route_contract_authority".to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "CodexHistoryItemStream TaskRoute::Docs DocsRouteState DocsRepair route_contract_pending docs_only_mutation_boundary generated_dependency_evidence_excluded RequestedWorkAuthoring_not_primary localized_docs_topic_completion docs_route_semantic_no_progress_guard docs_supporting_context_budget_exhausted_corrective_tool_output docs_budget_exhausted_recovery_surface_narrowed docs_budget_exhaustion_survives_partial_write docs_completed_deliverable_regression_rejected write_ready_prompt_projection".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "TaskRoute::Docs".to_string(),
                "DocsRouteState".to_string(),
                "DocsRepair".to_string(),
                "route_contract_pending".to_string(),
                "docs_only_mutation_boundary".to_string(),
                "generated_dependency_evidence_excluded".to_string(),
                "localized_docs_topic_completion".to_string(),
                "docs_route_semantic_no_progress_guard".to_string(),
                "docs_supporting_context_budget_exhausted_corrective_tool_output".to_string(),
                "docs_budget_exhausted_recovery_surface_narrowed".to_string(),
                "docs_budget_exhaustion_survives_partial_write".to_string(),
                "docs_completed_deliverable_regression_rejected".to_string(),
                "write_ready_prompt_projection".to_string(),
            ],
            forbidden_refs: vec![
                "case5_primary_key".to_string(),
                "filename_only_requested_work_authoring".to_string(),
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
            authority_source: "CanonicalHistoryItem PromptProjection current_lifecycle_state current_todo_focus_projection write_content_test_contract stable_provider_tool_schema provider_owned_tool_arguments positive_test_module_shape_contract observed_forbidden_marker_feedback unittest_main_test_content_allowed failed_write_content_shape_nonprogress sanitized_failed_write_tool_call_lifecycle summary_only_tool_call_replay stale_arguments_suppressed stale_payload_omitted stale_prelude_omitted stale_todo_progress_replay_omitted internal_control_items_not_provider_visible".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "current_lifecycle_state".to_string(),
                "current_todo_focus_projection".to_string(),
                "write_content_test_contract".to_string(),
                "stable_provider_tool_schema".to_string(),
                "summary_only_tool_call_replay".to_string(),
                "provider_owned_tool_arguments".to_string(),
                "provider_owned_tool_arguments".to_string(),
                "write_content_test_contract".to_string(),
                "positive_test_module_shape_contract".to_string(),
                "observed_forbidden_marker_feedback".to_string(),
                "unittest_main_test_content_allowed".to_string(),
                "failed_write_content_shape_nonprogress".to_string(),
                "sanitized_failed_write_tool_call_lifecycle".to_string(),
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
            authority_source: "CanonicalHistoryItem PromptProjection call_id_scoped_tool_call_output_pair model_arguments_replay_authority no_orphan_tool_output latest_user_input_preserved".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "call_id_scoped_tool_call_output_pair".to_string(),
                "model_arguments_replay_authority".to_string(),
                "no_orphan_tool_output".to_string(),
                "latest_user_input_preserved".to_string(),
            ],
            forbidden_refs: vec![
                "standalone_tool_output_without_call".to_string(),
                "provider_tool_role_without_assistant_tool_call".to_string(),
                "arguments_null_drops_tool_call".to_string(),
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
            authority_source: "CanonicalHistoryItem PromptProjection stale_inactive_authoring_pair_omitted non_executable_history_summary no_fake_executable_tool_arguments current_active_target_preserved no_orphan_tool_output".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "stale_inactive_authoring_pair_omitted".to_string(),
                "non_executable_history_summary".to_string(),
                "no_fake_executable_tool_arguments".to_string(),
                "current_active_target_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
            ],
            forbidden_refs: vec![
                "[omitted inactive authoring target]".to_string(),
                "[omitted stale inactive authoring payload".to_string(),
                "sentinel_path_as_tool_argument".to_string(),
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
            authority_source: "ToolLifecycleEnvelope RejectedToolProposal FunctionCallOutput success=false call_id_scoped_corrective_output allowed_surface terminal_guard".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "RejectedToolProposal".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "call_id_scoped_corrective_output".to_string(),
                "allowed_surface".to_string(),
                "terminal_guard".to_string(),
            ],
            forbidden_refs: vec!["tool_result_summary_parser_authority".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.no_content_write_is_no_progress"
                .to_string(),
            family: PreflightGateFamily::ToolProposalRejectionLifecycle,
            authority_source: "ToolLifecycleEnvelope FunctionCallOutput success=false progress_effect=no_progress no_content_change destructive_noop_acknowledgement_patch_rejected empty_apply_patch_hunks_rejected hunkless_update_patch_rejected zero_diff_patch_rejected artifact_preservation".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "progress_effect=no_progress".to_string(),
                "no_content_change".to_string(),
                "destructive_noop_acknowledgement_patch_rejected".to_string(),
                "empty_apply_patch_hunks_rejected".to_string(),
                "hunkless_update_patch_rejected".to_string(),
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
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring active_deliverable_targets ToolLifecycleEnvelope FunctionCallOutput success=false wrong_authoring_target progress_effect=no_progress terminal_guard stable_tool_schema".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "active_deliverable_targets".to_string(),
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_authoring_target".to_string(),
                "progress_effect=no_progress".to_string(),
                "terminal_guard".to_string(),
                "stable_tool_schema".to_string(),
            ],
            forbidden_refs: vec![
                "schema_const_payload_injection".to_string(),
                "content_changing_progress_outside_active_target".to_string(),
                "outer_timeout_on_repeated_wrong_target_write".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.verification_stable_tool_surface"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexHistoryItemStream ActiveWorkContract::Verification exact_shell_verification_authority repair_required_edit_surface shell_command_satisfaction FunctionCallOutput wrong_verification_command progress_effect=no_progress terminal_guard".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::Verification".to_string(),
                "exact_shell_verification_authority".to_string(),
                "repair_required_edit_surface".to_string(),
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
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring stable_tool_interface content_changing_satisfaction supporting_context progress_projection_saturation supporting_context_budget_recovery_surface FunctionCallOutput progress_effect=no_progress terminal_guard".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "stable_tool_interface".to_string(),
                "content_changing_satisfaction".to_string(),
                "supporting_context".to_string(),
                "progress_projection_saturation".to_string(),
                "supporting_context_budget_recovery_surface".to_string(),
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
            authority_source: "ActiveWorkContract::RequestedWorkAuthoring progress_projection_call_output todowrite_visible write_apply_patch_preserved authoring_supporting_context_budget_recovery_surface no_required_action_string stable_tool_schema semantic_no_progress_guard".to_string(),
            required_refs: vec![
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "progress_projection_call_output".to_string(),
                "todowrite_visible".to_string(),
                "write_apply_patch_preserved".to_string(),
                "authoring_supporting_context_budget_recovery_surface".to_string(),
                "stable_tool_schema".to_string(),
                "semantic_no_progress_guard".to_string(),
            ],
            forbidden_refs: vec![
                "required_next_action".to_string(),
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
            authority_source: "ToolRouter core_edit_tool_surface_registry_symmetry FunctionCallOutput wrong_authoring_target call_id_scoped_failed_inactive_write failed_inactive_tool_call_output_pair write_visible apply_patch_visible successful_stale_inactive_payload_summary_only".to_string(),
            required_refs: vec![
                "ToolRouter".to_string(),
                "core_edit_tool_surface_registry_symmetry".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_authoring_target".to_string(),
                "call_id_scoped_failed_inactive_write".to_string(),
                "failed_inactive_tool_call_output_pair".to_string(),
                "write_visible".to_string(),
                "apply_patch_visible".to_string(),
                "successful_stale_inactive_payload_summary_only".to_string(),
            ],
            forbidden_refs: vec![
                "anonymous_wrong_target_correction_only".to_string(),
                "hidden_core_tool".to_string(),
                "broad_write_saturation".to_string(),
                "failed_inactive_summary_only".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.rejected_tool_semantic_terminal_guard"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "RejectedToolProposal semantic_no_progress_key tool_allowed_false tool_choice_auto broad_surface_terminal_guard argument_payload_omitted projection_noise_absent".to_string(),
            required_refs: vec![
                "RejectedToolProposal".to_string(),
                "semantic_no_progress_key".to_string(),
                "tool_allowed_false".to_string(),
                "tool_choice_auto".to_string(),
                "broad_surface_terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "full_argument_hash".to_string(),
                "projection_id".to_string(),
                "required_singleton_only".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "HistoryItem ActiveWorkContract::Verification repair_required=true TurnControlEnvelope ActionAuthority tool_surface target repair_required_edit_surface".to_string(),
            required_refs: vec![
                "ActiveWorkContract".to_string(),
                "repair_required=true".to_string(),
                "ActionAuthority".to_string(),
                "repair_required_edit_surface".to_string(),
            ],
            forbidden_refs: vec![
                "continuation_shell_override".to_string(),
                "prompt_candidate_surface_authority".to_string(),
                "duplicate_success_cache".to_string(),
                "stale_read_or_shell_before_source_contract_repair".to_string(),
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
            authority_source: "FileChangeEvidence workspace_relative_path session_cwd_authority no_route_root_relative_closeout_target".to_string(),
            required_refs: vec![
                "FileChangeEvidence".to_string(),
                "workspace_relative_path".to_string(),
                "session_cwd_authority".to_string(),
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
            fixture_id: "fixture.turn_decision.codex_stable_tool_surface_authority"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "CodexResponsesRequest ActiveWorkContract RequestedWorkAuthoring candidate_tool_surface ActionAuthority stable_tool_schema tool_choice_auto open_work_lifecycle_evidence harness_closeout_guard open_obligation_final_message_guard".to_string(),
            required_refs: vec![
                "CodexResponsesRequest".to_string(),
                "ActiveWorkContract".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "ActionAuthority".to_string(),
                "stable_tool_schema".to_string(),
                "tool_choice_auto".to_string(),
                "open_work_lifecycle_evidence".to_string(),
                "harness_closeout_guard".to_string(),
                "open_obligation_final_message_guard".to_string(),
            ],
            forbidden_refs: vec![
                "required_action_string_grammar".to_string(),
                "schema_const_payload_injection".to_string(),
                "prompt_prose_only_write_authority".to_string(),
                "tool_choice_required_for_open_executable_work".to_string(),
                "text_only_final_clean_closeout_with_open_obligations".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.turn_decision.active_work_edit_before_verification_rerun"
                .to_string(),
            family: PreflightGateFamily::ControlEnvelopeProjection,
            authority_source: "CodexHistoryItemStream VerificationFailureCluster ActiveWorkContract Verification repair_required_active_work SourceViolatesContract ActionAuthority repair_lane_diagnostic_only stable_tool_schema call_id_scoped_outputs".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "VerificationFailureCluster".to_string(),
                "ActiveWorkContract".to_string(),
                "Verification".to_string(),
                "repair_required_active_work".to_string(),
                "SourceViolatesContract".to_string(),
                "ActionAuthority".to_string(),
                "repair_lane_diagnostic_only".to_string(),
                "stable_tool_schema".to_string(),
                "call_id_scoped_outputs".to_string(),
            ],
            forbidden_refs: vec![
                "stale_shell_rerun_before_repair_edit".to_string(),
                "repair_lane_top_level_override".to_string(),
                "prompt_surface_only_tool_authority".to_string(),
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
            authority_source: "CodexTurnComplete StopRequest hook_prompt_message text_only_hook_prompt RuntimeCompleted final_assistant_message OpenObligation ManualStCloseoutEvidence CloseoutContinuationUserTurn missing_artifacts file_changing_tool_call_required latest_verification_command_evidence bounded_route_failure".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "StopRequest".to_string(),
                "hook_prompt_message".to_string(),
                "text_only_hook_prompt".to_string(),
                "RuntimeCompleted".to_string(),
                "final_assistant_message".to_string(),
                "OpenObligation".to_string(),
                "ManualStCloseoutEvidence".to_string(),
                "CloseoutContinuationUserTurn".to_string(),
                "missing_artifacts".to_string(),
                "file_changing_tool_call_required".to_string(),
                "latest_verification_command_evidence".to_string(),
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
            authority_source: "CodexHistoryItemStream FunctionCallOutput VerificationFailed evidence_labels requested_work_authoring_targets deliverable_artifact_paths ManualStCloseoutEvidence verification_repair_hook stable_tool_schema no_tool_closeout_prevention".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "FunctionCallOutput".to_string(),
                "VerificationFailed".to_string(),
                "evidence_labels".to_string(),
                "requested_work_authoring_targets".to_string(),
                "deliverable_artifact_paths".to_string(),
                "ManualStCloseoutEvidence".to_string(),
                "verification_repair_hook".to_string(),
                "stable_tool_schema".to_string(),
            ],
            forbidden_refs: vec![
                "test_method_symbol_as_deliverable".to_string(),
                "verification_label_requested_work_target".to_string(),
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
            fixture_id: "fixture.manual_st.route_evidence_schema".to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "route_manifest verification_command_log workspace_diff_manifest request_payload_summary timeout_classification explicit_session_continuation no_continue_last_with_session provider_stream_idle_timeout_classification provider_transport_stream_error_classification semantic_no_progress_terminal_classification".to_string(),
            required_refs: vec![
                "route_manifest".to_string(),
                "verification_command_log".to_string(),
                "workspace_diff_manifest".to_string(),
                "explicit_session_continuation".to_string(),
                "no_continue_last_with_session".to_string(),
                "provider_stream_idle_timeout_classification".to_string(),
                "provider_transport_stream_error_classification".to_string(),
                "semantic_no_progress_terminal_classification".to_string(),
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
        "verification_command_log.json".to_string(),
        "workspace_diff_manifest.json".to_string(),
        "result.json".to_string(),
        "preflight_report.json".to_string(),
        "timeout_classification.json".to_string(),
    ]
}

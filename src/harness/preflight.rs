use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::RuntimeError;

const PREFLIGHT_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const PREFLIGHT_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";
const PREFLIGHT_FIXTURE_CONTEXT_WINDOW: u32 = 131_072;
const PREFLIGHT_FIXTURE_MAX_OUTPUT_TOKENS: u32 = 8_192;
const BANNED_SCOPE_SHRINK_TERMS: &[&str] = &[
    "MVP",
    "最小構成",
    "最小限",
    "簡略化",
    "簡略版",
    "まずはここまで",
    "後で拡張",
];

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
    FailureRegistryAuthority,
    DesignAuthority,
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

    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && !crate::protocol::protocol_store_latest_turn_position_uses_unified_item_stream_fixture_passes(
        )
    {
        diagnostics.push(
            "protocol store latest-turn position can still use table precedence instead of a unified protocol item stream candidate set".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && !crate::protocol::protocol_store_latest_turn_position_resists_timestamp_drift_fixture_passes(
        )
    {
        diagnostics.push(
            "protocol store latest-turn position can still use wall-clock timestamps as primary continuation authority instead of event-sourced append order".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && !crate::protocol::protocol_store_single_item_append_order_atomic_commit_fixture_passes()
    {
        diagnostics.push(
            "protocol store single-item appends can still commit canonical items and append-order authority outside one repository-owned transaction".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && !crate::runtime::event_bus::run_event_publisher_tolerates_observer_absence_fixture_passes(
        )
    {
        diagnostics.push(
            "runtime event projection fanout can still make observer absence a control-plane failure".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && !crate::harness::runtime_writer::native_harness_recorder_is_harness_only_fixture_passes()
    {
        diagnostics.push(
            "native harness recording can still own protocol persistence instead of running as a harness-only subscriber under ProtocolRecordingSink".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.full_access_configured_boundary_authority"
        && !crate::tool::context::full_access_configured_boundary_fixture_passes()
    {
        diagnostics.push(
            "full_access can still auto-allow outside-workspace, network, or protected workspace authority requests instead of staying bounded by configured workspace authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.external_tool_surface_schema_validation"
        && !crate::mcp::mcp_tools_list_rejects_malformed_tool_descriptors_fixture_passes()
    {
        diagnostics.push(
            "MCP tools/list parsing can still silently drop malformed descriptors instead of failing closed on external tool-surface schema drift".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !preflight_gate_suite_docs_component_widget_fixture_authority_absent_fixture_passes()
    {
        diagnostics.push(
            "PreflightGateSuite can still present component/widget filenames as generic active fixture authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !preflight_gate_suite_docs_component_arcade_fixture_payload_authority_absent_fixture_passes()
    {
        diagnostics.push(
            "PreflightGateSuite can still allow component/widget/arcade filenames as generic fixture payload authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !preflight_gate_suite_docs_widget_generated_test_payload_authority_absent_fixture_passes(
        )
    {
        diagnostics.push(
            "PreflightGateSuite can still allow test_widget.py as generic generated-test fixture payload authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !preflight_gate_suite_docs_marker_full_workflow_neutral_scope_fixture_passes()
    {
        diagnostics.push(
            "PreflightGateSuite workflow-neutral marker summary can still omit active-fixture, generic payload, or generated-test grounding scope".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !testing_small_docs_current_product_authority_fixture_passes()
    {
        diagnostics.push(
            "Small docs/testing authority documents can still retain lowercase moyai product authority in current NG handling or rebuild rules".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.flow_contract_harness_authority"
        && !flow_contract_harness_map_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Flow/contract/harness responsibility map can still retain stale manual ST path casing, obsolete comparison surfaces, or representative case labels as current design authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.basic_design_authority"
        && !basic_design_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Basic Design can still retain lowercase product/path authority, phase-era CLI-first construction wording, stale provider defaults, exact tool-surface lists, or implementation-handoff wording instead of current moyAI Codex-style typed lifecycle / single-control-plane / Desktop + harness architecture authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.feature_inventory_authority"
        && !feature_inventory_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Feature Inventory can still retain lowercase product/path authority, phase-era handoff wording, stale provider defaults, exact tool-surface lists, or CLI-first phase sequencing instead of current moyAI capability taxonomy / typed lifecycle / route-owned verification authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.desktop_app_basic_design_authority"
        && !desktop_app_basic_design_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Desktop App Basic Design can still retain lowercase product/path authority, exact build/check command authority, dev-server/executable specifics, old UI cleanup wording, or layout-specific authority instead of current moyAI Desktop/App typed adapter and projection boundaries".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.desktop_app_detailed_design_authority"
        && !desktop_app_detailed_design_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Desktop App Detailed Design can still retain lowercase product/path authority, provider/backend-specific probes, exact build/test commands, executable/Tauri packaging specifics, UI layout specifics, implementation order, or representative route steps instead of current moyAI typed Desktop/App adapter contracts".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.tui_design_authority"
        && !tui_design_current_authority_fixture_passes()
    {
        diagnostics.push(
            "TUI Design can still retain lowercase product/path authority, phase-era implementation order, stale provider profile, exact module/crate/screen/tool lists, or reference UI details instead of current moyAI typed terminal adapter contracts".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.agent_harness_architecture_authority"
        && !agent_harness_architecture_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Agent Harness Architecture can still retain exact write/patch/read, exact command/rerun, or Python unittest scenario evidence as current architecture authority instead of typed action-family and route-owned verification evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.agent_harness_components_authority"
        && !agent_harness_components_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Agent Harness Components can still retain lowercase moyai product authority in current component design prose instead of current moyAI Agent Harness Engine authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.agent_state_machine_authority"
        && !agent_state_machine_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Agent State Machine can still retain lowercase product/path authority, exact tool/command/rerun authority, Python scenario evidence, or stale comparison labels instead of typed lifecycle and route-owned verification evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.agent_harness_implementation_authority"
        && !agent_harness_implementation_design_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Agent Harness Implementation Design can still retain lowercase product/path/CLI authority, legacy compatibility framing, provider/shell replay wording, pre-policy case evidence, or stale open-work status instead of current moyAI typed event-log harness authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.typed_contract_inventory_authority"
        && !typed_contract_inventory_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Typed contract inventory can still retain stale moyai path casing, LM Studio-specific provider preflight payload authority, case-primary scenario contract version sources, or exact language-specific verification command concern authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.root.spec_current_authority"
        && !root_specs_current_product_path_authority_fixture_passes()
    {
        diagnostics.push(
            "README.md / ProjectBrief.md can still retain lowercase moyai product/path authority instead of current moyAI root specification authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.thread_turn_item_protocol_authority"
        && !thread_turn_item_protocol_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Thread/Turn/Item protocol can still retain stale moyai path casing, RequiredAction string grammar, phase-era skeleton/migration wording, obsolete control-envelope gate names, or future connection-order status as current protocol authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.turn_decision_pipeline_authority"
        && !turn_decision_pipeline_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Turn Decision Pipeline can still retain lowercase current-product, FR-numbered, exact action-string, language/test-runner-specific, or domain-specific current authority instead of current moyAI typed lifecycle / action-family / adapter-owned evidence / workflow-neutral invariant authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.runtime_contracts_authority"
        && !runtime_contracts_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Runtime Contracts can still retain stale current-build moyai product authority or language-specific repair/correction authority instead of current moyAI runtime contract and adapter-owned evidence authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.verification_harness_authority"
        && !verification_harness_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Verification Harness design can still retain stale moyai product/path authority instead of current moyAI harness authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.item_lifecycle_detail_authority"
        && !item_lifecycle_detail_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Item Lifecycle Detail Design can still retain stale moyai product/path authority instead of current moyAI target lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.tiered_quality_gates_authority"
        && !tiered_quality_gates_route_taxonomy_invariant_authority_fixture_passes()
    {
        diagnostics.push(
            "Tiered Quality Gates can still retain behavior-stopper or case-primary quality-gate authority instead of route taxonomy and invariant/artifact-role authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.current_authority_index_authority"
        && !current_authority_index_current_product_authority_fixture_passes()
    {
        diagnostics.push(
            "Current Authority Index can still retain lowercase current-product moyai authority instead of current moyAI authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.codex_control_plane_redesign_expanded_authority"
        && !codex_control_plane_redesign_expanded_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex Control-Plane Redesign Expanded Review can still retain lowercase product authority, exact tool/provider/action surfaces, FR2/case2b current framing, implementation-date slices, OpenClaw comparison priority drift, or old AgentLoop rebuild wording as current control-plane authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.codex_derived_redesign_recommendations_authority"
        && !codex_derived_redesign_recommendations_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex-derived Redesign Recommendations can still retain future-phase rebuild sequencing, lowercase product authority, exact implementation examples, provider/profile examples, or rebuild-vs-incremental wording instead of current moyAI adopted protocol-first runtime authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.codex_ui_adoption_review_authority"
        && !codex_ui_adoption_review_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex UI Adoption Review can still retain dated screenshot audit notes, absolute screenshot paths, stale presentation-layer wording, local search/grep wording, implementation-history bullets, or raw verification commands instead of current moyAI Desktop/App typed projection authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.codex_lifecycle_fr03_gap_analysis_authority"
        && !codex_lifecycle_fr03_gap_analysis_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex Lifecycle FR03 Gap Analysis can still retain lowercase product authority, exact FR/case/tool/action/file/type examples, mixed implementation-recipe wording, or next-iteration sequencing instead of current moyAI rejected-proposal and candidate-repair lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.codex_itemlifecycle_authority"
        && !codex_itemlifecycle_survey_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex Item Lifecycle Survey can still retain historical FR/case/tool/file/provider incident ledger wording instead of current moyAI Thread / Turn / Item lifecycle survey authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.codex_reference_comparison_authority"
        && !codex_reference_comparison_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex Reference Comparison can still retain dated FR/case/tool/file/provider incident comparison wording instead of current moyAI multi-reference lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.codex_structure_map_authority"
        && !codex_structure_map_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Codex Structure Map can still retain source-path ledger wording, exact type/tool primary keys, or stale current-state claims instead of current moyAI Codex structure authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.contract_comparison_authority"
        && !contract_comparison_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Contract Comparison can still retain lowercase product authority, stale runtime owner paths, exact tool/action names, Python verification commands, provider/profile examples, or case artifact incident-ledger wording instead of current moyAI Codex-first contract comparison authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.harness_comparison_authority"
        && !harness_comparison_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Harness Comparison can still retain lowercase product authority, stale implementation paths, fixed route ordering, exact manual-ST artifacts, Python verification commands, provider/profile examples, or search-tool wording instead of current moyAI harness authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.opencode_structure_map_authority"
        && !opencode_structure_map_current_authority_fixture_passes()
    {
        diagnostics.push(
            "opencode Structure Map can still retain lowercase product authority, phase-era sequencing, stale scope decisions, exact opencode source paths, exact tool/module names, or search-tool wording instead of current moyAI opencode reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.opencode_flow_description_authority"
        && !opencode_flow_description_current_authority_fixture_passes()
    {
        diagnostics.push(
            "opencode Flow Description can still retain lowercase product authority, exact source paths, exact tool/module names, provider/prompt-family examples, dated incident comparison, case labels, or exact completion/todo/tool surfaces instead of current moyAI opencode flow authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.roocode_flow_description_authority"
        && !roocode_flow_description_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Roo Code Flow Description can still retain exact source paths, exact tool/class names, provider examples, dated incident comparison, lowercase product authority, or reserved adoption notes instead of current moyAI Roo Code recovery-flow reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.moyai_flow_description_authority"
        && !moyai_flow_description_current_authority_fixture_passes()
    {
        diagnostics.push(
            "moyAI Flow Description can still retain lowercase product authority, exact source paths, legacy AgentLoop ownership, dated case evidence, exact commands, exact tool names, or incident-specific repair narratives instead of current moyAI runtime flow authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.opencode_contract_authority"
        && !opencode_contract_current_authority_fixture_passes()
    {
        diagnostics.push(
            "opencode Contract can still retain lowercase product authority, exact opencode source paths, exact tool/module names, provider examples, dated incident comparison, or reading-order source lists instead of current moyAI opencode contract reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.roocode_contract_authority"
        && !roocode_contract_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Roo Code Contract can still retain exact Roo source paths, exact tool/class names, provider examples, dated incident comparison, lowercase product authority, category adoption notes, or reading-order source lists instead of current moyAI Roo Code recovery contract reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.opencode_verification_harness_authority"
        && !opencode_verification_harness_current_authority_fixture_passes()
    {
        diagnostics.push(
            "opencode Verification Harness can still retain exact opencode test source paths, exact test filenames, exact tool names, dated incident comparison, lowercase product authority, named scenario routes, or reading-order source lists instead of current moyAI opencode deterministic harness reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.roocode_verification_harness_authority"
        && !roocode_verification_harness_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Roo Code Verification Harness can still retain exact Roo source paths, exact integration/unit test filenames, exact tool names, dated incident comparison, lowercase product authority, named scenario routes, local provider artifact wording, or reading-order source lists instead of current moyAI Roo Code recovery harness reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.survey.openclaw_runtime_authority"
        && !openclaw_runtime_survey_current_authority_fixture_passes()
    {
        diagnostics.push(
            "OpenClaw Runtime Survey can still retain exact OpenClaw source paths, implementation names, provider/model examples, dated FR/case cluster mapping, adopted-difference work instructions, or reading-source lists instead of current moyAI OpenClaw runtime reference authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.replay_first_harness_authority"
        && !replay_first_harness_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Replay-first Harness can still retain lowercase current-product/path authority, case-primary replay fixture/stopper authority, exact unittest rerun summary authority, or case-label evidence authority instead of route-neutral invariant replay authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.design.run_store_event_log_authority"
        && !run_store_event_log_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Run Store / Event Log design can still retain lowercase current-product, prompt-string correction, representative-scenario, exact Python verification lane, incident-specific tool repetition, behavior blocker, verification rerun lane, or unittest output authority instead of typed event-sourced route authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_header_current_entry_schema_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry header can still omit current FR22 registration scope or why-why boundary fields".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_pending_status_verified_evidence_consistent_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry entries can still keep pending lower-tier status while claiming verified regression/preflight evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_implemented_pending_status_verified_evidence_consistent_fixture_passes(
        )
    {
        diagnostics.push(
            "FailureRegistry implemented-pending-verification statuses can still claim verified regression evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_status_pending_plan_consistent_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry entries can still claim verified root-fix status while retaining pending regression-plan wording".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_status_future_action_plan_consistent_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry entries can still claim verified root-fix status while retaining future-action regression-plan wording".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_status_harness_assessment_current_lifecycle_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified/root-fix entries can still retain pre-fix gap or future-action harness assessment wording".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_rerun_exposed_status_verified_lifecycle_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry rerun-exposed statuses can still omit verified root-fix lifecycle projection or retain rerun-pending regression text".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_status_exposed_id_matches_next_failure_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified fresh-sweep exposed statuses can still point at a different failure id than the next registered post-fix failure".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_rerun_pending_status_matches_successor_evidence_fixture_passes(
        )
    {
        diagnostics.push(
            "FailureRegistry verified rerun-pending statuses can still outlive later post-fix rerun evidence that registered the successor failure".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_next_failure_exposed_status_names_successor_id_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry next-failure-exposed statuses can still omit the exposed successor id"
                .to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_rerun_status_cannot_remain_transient_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified root-fix rerun statuses can still remain as transient pending or in-progress lifecycle projections".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_pending_fresh_rerun_status_requires_no_successor_evidence_fixture_passes(
        )
    {
        diagnostics.push(
            "FailureRegistry pending fresh-rerun statuses can still outlive successor post-fix rerun evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_post_fix_verified_status_requires_successor_projection_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry post_fix_verified statuses can still hide adjacent successor rerun evidence instead of naming the exposed successor id".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_pending_status_blocker_resolution_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified-pending statuses can still outlive resolved blocker or rerun-exposed lifecycle evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_root_identified_status_successor_evidence_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry root-identified statuses can still outlive adjacent successor evidence"
                .to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_root_fix_in_progress_status_successor_evidence_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry root-fix pending/in-progress statuses can still outlive later successor evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_verified_status_pending_investigation_projection_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified/root-fix entries can still retain pending investigation/root-fix prose or malformed Markdown block ownership".to_string(),
        );
    }
    if gate.gate_id == "preflight.harness.failure_registry_projection_sync"
        && !failure_registry_regression_fixture_authority_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "FailureRegistry verified/root-fix regression projections can still retain workflow-specific fixture payload authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::manual_st::manual_st_provider_retry_exhausted_timeout_classification_fixture_passes()
    {
        diagnostics.push(
            "provider stream retry exhaustion can still be classified without first-class provider-boundary owner/evidence refs in timeout_classification.json".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema" {
        let failures =
            crate::harness::manual_st::manual_st_closeout_and_route_fixture_workflow_neutral_failures(
            );
        if !failures.is_empty() {
            diagnostics.push(format!(
                "manual ST closeout and route-progress fixtures can still use legacy case/language/provider authority instead of workflow-neutral current-profile evidence: {}",
                failures.join(", ")
            ));
        }
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !manual_st_reference_exports_scope_hygiene_fixture_passes()
    {
        diagnostics.push(
            "manual ST spec/reference artifacts can still preserve scope-shrinking wording as comparison authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !testing_metadata_current_guard_index_fixture_passes()
    {
        diagnostics.push(
            "testing metadata can still omit current active deterministic convergence guards from the active metadata index".to_string(),
        );
    }

    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes()
    {
        diagnostics.push(
            "content-changing control projection text still conflates available tools with satisfying file-change progress".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::app::run_service::app_initial_turn_route_key_projection_fixture_passes()
    {
        diagnostics.push(
            "app initial TurnContext can still project active_work_kind from Rust Debug route spelling instead of stable TaskRoute keys".to_string(),
        );
    }

    if gate.gate_id == "preflight.protocol.history_item_lifecycle_authority"
        && !crate::protocol::filechange_item_projection_preserves_call_id_fixture_passes()
    {
        diagnostics.push(
            "canonical HistoryItem / TurnItem FileChange projections can still drop the owning tool call id while RuntimeEvent FileChangesRecorded keeps it".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.history_item_lifecycle_authority"
        && !crate::protocol::tool_output_projection_preserves_blocked_action_fixture_passes()
    {
        diagnostics.push(
            "canonical HistoryItem / RuntimeEvent ToolOutput projections can still drop blocked-action authority from typed ToolFeedbackEnvelope metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.history_item_lifecycle_authority"
        && !crate::protocol::pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes(
        )
    {
        diagnostics.push(
            "pending ToolLifecycle projection can still fabricate blocked-action evidence from display-only tool-call titles before any tool result or rejection exists".to_string(),
        );
    }
    if gate.gate_id == "preflight.protocol.history_item_lifecycle_authority"
        && !crate::app::run_service::app_resume_latest_user_sequence_primary_order_fixture_passes()
    {
        diagnostics.push(
            "app no-prompt resume latest-user selection can still use timestamp-primary ordering instead of canonical HistoryItem.sequence_no order".to_string(),
        );
    }

    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::active_apply_patch_target_projection_renders_operation_template_fixture_passes()
    {
        diagnostics.push(
            "active apply_patch target projection still lacks a concrete current-item patch operation template".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::edit_only_authoring_grounding_recovery_narrows_action_surface_fixture_passes()
    {
        diagnostics.push(
            "edit-only authoring grounding recovery can still compile a required edit action while leaving supporting tools executable in the same TurnControlEnvelope".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::non_python_edit_projection_uses_language_adapter_fixture_passes()
    {
        diagnostics.push(
            "required edit content-shape projection still depends on Python-specific suffix branches instead of language adapter artifact roles".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::conflicting_required_actions_fail_closed_fixture_passes()
    {
        diagnostics.push(
            "conflicting explicit required actions can still be collapsed and replaced by singleton inferred action authority instead of failing closed before dispatch".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::turn_obligation_required_actions_are_typed_fixture_passes()
    {
        diagnostics.push(
            "TurnObligation required actions are still carried as legacy string grammar instead of typed RequiredAction state".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::required_action_projection_label_is_typed_rendering_fixture_passes()
    {
        diagnostics.push(
            "RequiredAction projection labels can still be sourced from cached projection_text instead of typed action fields".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::unavailable_explicit_required_action_fails_closed_fixture_passes()
    {
        diagnostics.push(
            "explicit RequiredAction whose tool is unavailable can still be replaced by singleton fallback instead of failing closed with RequiredActionToolNotAllowed".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::projection_bundle_lifecycle_fields_match_authority_fixture_passes()
    {
        diagnostics.push(
            "ProjectionBundle surfaces can still carry stale required actions or lifecycle refs that differ from ActionAuthority and open obligations".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::named_tool_choice_matches_required_action_fixture_passes()
    {
        diagnostics.push(
            "named tool_choice can still force a different provider tool than the compiled explicit RequiredAction".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::action_authority_matches_open_obligations_fixture_passes()
    {
        diagnostics.push(
            "ActionAuthority can still carry stale required action or surface state that differs from TurnContext and open obligations".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::active_work_contract_matches_open_obligation_targets_fixture_passes()
    {
        diagnostics.push(
            "ActiveWorkContract targets or operation intents can still diverge from open obligation authority while dispatch remains internally self-consistent".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::verification_active_work_matches_open_obligation_targets_fixture_passes(
        )
    {
        diagnostics.push(
            "verification-only ActiveWorkContract targets can still diverge from open verification obligation targets while shell command authority remains internally self-consistent".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::active_work_contract_route_phase_matches_turn_context_fixture_passes()
    {
        diagnostics.push(
            "ActiveWorkContract route or process_phase can still diverge from TurnContext lifecycle owner fields while target and command authority remain internally self-consistent".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::allowed_forbidden_tool_surfaces_are_disjoint_fixture_passes()
    {
        diagnostics.push(
            "allowed and forbidden tool surfaces can still overlap while every TurnControlEnvelope projection carries the same internally contradictory provider surface".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::turn_decision_projection_matches_control_envelope_fixture_passes()
    {
        diagnostics.push(
            "TurnDecisionDiagnostic can still diverge from TurnContext, ActiveWorkContractProjection, or ActionAuthority while request diagnostics and control_projection obligations preserve stale lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::continuation_contract_matches_control_envelope_fixture_passes()
    {
        diagnostics.push(
            "ContinuationContract can still diverge from TurnContext or ActiveWorkContractProjection while typed compaction/handoff state preserves stale lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::output_contract_final_answer_matches_open_obligations_fixture_passes()
    {
        diagnostics.push(
            "OutputContract can still require final assistant text while current open obligations require tool-mediated progress".to_string(),
        );
    }
    if gate.gate_id == "preflight.control_envelope.dispatch_projection_authority"
        && !crate::protocol::repair_target_identity_aliases_compile_exact_write_action_fixture_passes(
        )
    {
        diagnostics.push(
            "repair-target alias control-envelope projection can still compile ambiguous or alias-derived targets into an exact write action instead of preserving singleton typed action authority".to_string(),
        );
    }

    if matches!(gate.family, PreflightGateFamily::ProtocolItemLifecycle)
        && (!protocol_item_lifecycle_fixture_passes()
            || !crate::protocol::history_item_projection_roles_are_not_authority_fixture_passes()
            || !crate::protocol::turn_item_internal_projection_roles_are_not_primary_display_fixture_passes()
            || !crate::agent::prompt::provider_replay_uses_protocol_visibility_roles_fixture_passes(
            )
            || !crate::agent::state::state_reducer_ignores_projection_cache_items_fixture_passes())
    {
        diagnostics.push(
            "canonical protocol item lifecycle does not preserve effective tool arguments, typed file change evidence, typed tool output success, or the provider/state authority boundary for projection/control/cache items".to_string(),
        );
    }

    if gate.gate_id == "preflight.protocol.persistence_unit_of_work_authority"
        && (!protocol_persistence_unit_of_work_fixture_passes()
            || !crate::storage::session_repo::protocol_message_parts_use_single_unit_of_work_fixture_passes()
            || !crate::storage::session_repo::todo_update_uses_single_unit_of_work_fixture_passes()
            || !crate::storage::session_repo::tool_output_filechange_projection_single_unit_of_work_fixture_passes()
            || !crate::storage::session_repo::tool_output_filechange_projection_owner_coherence_fixture_passes()
            || !crate::app::run_service::resume_latest_user_message_uses_item_order_fixture_passes(
            )
            || !crate::agent::loop_impl::terminal_token_accounting_sequence_fixture_passes()
            || !crate::protocol::pre_recorded_protocol_sequence_reservation_fixture_passes()
            || !crate::protocol::protocol_store_rejects_incoherent_event_bundles_fixture_passes()
            || !crate::session::service::stale_running_cleanup_records_protocol_terminal_fixture_passes())
    {
        diagnostics.push(
            "session storage can persist compatibility messages, message parts, todo graph replacement, content-changing ToolOutput/FileChange projection, session status, stale-running terminal cleanup, and protocol runtime projection through separate authorities, content-changing completion can mix mismatched ToolOutput/FileChange owners inside one unit-of-work, protocol store can admit mismatched runtime/history/turn bundles, resume can select the latest user message by raw vector order instead of canonical item order, or pre-recorded protocol events can reuse a sequence number before the sink observes them"
                .to_string(),
        );
    }

    if gate.gate_id == "preflight.harness.failure_registry_projection_sync" {
        let failed_registry_checks = [
            (
                "failure_registry_markdown_json_sync",
                failure_registry_markdown_json_sync_fixture_passes(),
            ),
            (
                "failure_registry_markdown_json_status_parity",
                failure_registry_markdown_json_status_parity_fixture_passes(),
            ),
            (
                "failure_registry_implemented_pending_status_verified_evidence_consistent",
                failure_registry_implemented_pending_status_verified_evidence_consistent_fixture_passes(
                ),
            ),
            (
                "failure_registry_verified_status_future_action_plan_consistent",
                failure_registry_verified_status_future_action_plan_consistent_fixture_passes(),
            ),
            (
                "failure_registry_verified_status_exposed_id_matches_next_failure",
                failure_registry_verified_status_exposed_id_matches_next_failure_fixture_passes(),
            ),
            (
                "failure_registry_verified_rerun_pending_status_matches_successor_evidence",
                failure_registry_verified_rerun_pending_status_matches_successor_evidence_fixture_passes(
                ),
            ),
            (
                "failure_registry_next_failure_exposed_status_names_successor_id",
                failure_registry_next_failure_exposed_status_names_successor_id_fixture_passes(),
            ),
            (
                "failure_registry_verified_rerun_status_cannot_remain_transient",
                failure_registry_verified_rerun_status_cannot_remain_transient_fixture_passes(),
            ),
            (
                "failure_registry_pending_fresh_rerun_status_requires_no_successor_evidence",
                failure_registry_pending_fresh_rerun_status_requires_no_successor_evidence_fixture_passes(),
            ),
            (
                "failure_registry_root_identified_status_successor_evidence",
                failure_registry_root_identified_status_successor_evidence_fixture_passes(),
            ),
            (
                "failure_registry_root_fix_in_progress_status_successor_evidence",
                failure_registry_root_fix_in_progress_status_successor_evidence_fixture_passes(),
            ),
            (
                "failure_registry_verified_status_harness_assessment_current_lifecycle",
                failure_registry_verified_status_harness_assessment_current_lifecycle_fixture_passes(),
            ),
            (
                "failure_registry_regression_fixture_authority_workflow_neutral",
                failure_registry_regression_fixture_authority_workflow_neutral_fixture_passes(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, passed)| (!passed).then_some(name))
        .collect::<Vec<_>>();
        if !failed_registry_checks.is_empty() {
            diagnostics.push(
                "Failure Registry Markdown and JSON projections diverge for active FR22 ids/status values, full Markdown/JSON id/status sequence parity, implemented-pending-verification entries still claim verified regression evidence, verified root-fix entries still retain future-action regression-plan wording, fresh-sweep exposed statuses drift from the next registered failure, verified rerun-pending statuses outlive successor evidence, next-failure-exposed statuses omit successor ids, verified rerun statuses retain transient pending/in-progress lifecycle wording, pending fresh-rerun statuses outlive successor evidence, root-identified statuses outlive adjacent successor evidence, root-fix pending/in-progress statuses outlive successor evidence, verified harness assessments retain pre-fix gap/future-action wording, or registry regression fixture authority remains workflow-specific".to_string(),
            );
            diagnostics.push(format!(
                "failed Failure Registry projection fixtures: {}",
                failed_registry_checks.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::gate::schema::event_stream_identity_coherence_fixture_passes()
    {
        diagnostics.push(
            "harness replay schema gate can still accept mixed run_id, duplicate event id, duplicate sequence_no, or non-monotonic event streams before downstream replay gates run".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::manual_st::manual_st_closeout_repair_targets_preserve_exact_identity_fixture_passes()
    {
        diagnostics.push(
            "manual ST closeout repair-target projection can still synthesize suffix or basename identities instead of exact route artifact targets".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::manual_st::manual_st_verification_commands_are_generic_public_commands_fixture_passes()
    {
        diagnostics.push(
            "manual ST verification command extraction can still omit generic public shell commands before route-owned verification runs".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::manual_st::manual_st_route_preflight_report_codex_style_admission_fixture_passes()
    {
        diagnostics.push(
            "manual ST route startup can still accept fabricated or status-only preflight reports instead of requiring Codex-style active preflight identity and result evidence".to_string(),
        );
    }

    if gate.gate_id == "preflight.item_lifecycle.provider_replay_call_output_symmetry"
        && (!provider_replay_call_output_symmetry_fixture_passes()
            || !crate::agent::prompt::provider_replay_sequence_order_resists_timestamp_drift_fixture_passes()
            || !crate::agent::event::stream_accumulator_complete_tool_call_lifecycle_fixture_passes())
    {
        diagnostics.push(
            "provider replay is not built from canonical HistoryItem call/output pairs, orphan/error items can still become assistant text, timestamp drift can still reorder provider-visible replay before canonical sequence order, or incomplete provider stream tool-call deltas can still become completed tool calls".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::desktop::startup::DesktopStartupState::desktop_startup_fixture_current_provider_profile_fixture_passes()
    {
        diagnostics.push(
            "Desktop startup fixtures can still project stale localhost provider profile instead of the current closed-network OpenAI-compatible startup readiness profile".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::desktop::web_model::desktop_web_model_fixture_current_provider_profile_domain_neutral_fixture_passes()
    {
        diagnostics.push(
            "Desktop web-model fixtures can still project stale localhost provider profile or domain-specific dependency setup text as generic UI authority".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::desktop::app::desktop_app_current_provider_profile_fixture_passes()
    {
        diagnostics.push(
            "Desktop app fixtures can still project stale localhost provider profile or example-model metadata as generic UI/export authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.llm_transport.stream_retry_before_first_event"
        && (!crate::llm::openai_compat::stream_event_retry_classifier_fixture_passes()
            || !crate::llm::openai_compat::stream_idle_timeout_retry_exhaustion_error_fixture_passes(
            )
            || !crate::llm::openai_compat::streaming_tool_call_projection_uses_delta_index_stable_ids_fixture_passes()
            || !crate::llm::openai_compat::streaming_tool_call_late_name_preserves_typed_tool_identity_fixture_passes()
            || !crate::tui::prompt_enhance::prompt_enhance_sink_excludes_reasoning_delta_fixture_passes()
            || !crate::agent::loop_impl::request_diagnostics_stream_retry_policy_fixture_passes())
    {
        diagnostics.push(
            "provider SSE decode/transport failures or stream idle timeouts before the first emitted model event are not classified as retryable, retry exhaustion is not terminal evidence, streaming tool-call projection can split or collide call ids, streaming arguments-before-name can fix the tool identity as unknown, reasoning deltas can become visible prompt text, request diagnostics omit stream retry policy, or non-transport/post-partial-output stream errors are retryable"
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

    if gate.gate_id == "preflight.docs_spec.semantic_reconciliation_before_handoff"
        && !crate::agent::docs_semantic_contract::docs_semantic_documentation_target_classifier_shape_based_fixture_passes()
    {
        diagnostics.push(
            "docs semantic documentation target classification can still depend on exact README/design/basic/detail filename branches instead of documentation artifact shape".to_string(),
        );
    }

    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && (!desktop_transcript_primary_reading_fixture_passes()
            || !crate::session::markdown::codex_turn_block_markdown_fixture_passes()
            || !crate::session::transcript::transcript_from_history_items_uses_item_sequence_fixture_passes()
            || !crate::cli::render::cli_history_renderer_uses_canonical_transcript_projection_fixture_passes()
            || !crate::cli::render::cli_history_renderer_ignores_compatibility_transcript_fixture_passes()
            || !crate::cli::render::cli_json_history_renderer_respects_reasoning_visibility_fixture_passes()
            || !crate::cli::render::cli_human_renderer_typed_lifecycle_projection_fixture_passes()
            || !crate::tui::query::session_view_rejects_empty_canonical_history_fixture_passes()
            || !crate::tui::state::tui_turn_item_projection_uses_turn_local_sequence_fixture_passes()
            || !crate::tui::state::tui_primary_transcript_omits_internal_projection_items_fixture_passes()
            || !crate::session::service::session_service_current_provider_profile_fixture_passes()
            || !crate::session::markdown::filechange_display_export_preserves_call_id_fixture_passes()
            || !crate::session::markdown::session_markdown_legacy_toolcall_arguments_do_not_render_typed_projection_fixture_passes()
            || !crate::desktop::query::desktop_pseudo_tool_call_closeout_evidence_preserved_fixture_passes()
            || !crate::desktop::query::desktop_query_current_provider_profile_fixture_passes()
            || !crate::desktop::query::desktop_query_todo_status_typed_projection_fixture_passes()
            || !crate::desktop::web_model::desktop_gui_typed_visibility_projection_fixture_passes()
            || !crate::desktop::state::desktop_state_current_provider_profile_fixture_passes()
            || !crate::desktop::models::desktop_session_row_status_typed_projection_fixture_passes()
            || !crate::desktop::app::desktop_open_transcript_markdown_preserves_visible_evidence_fixture_passes()
            || !crate::desktop::artifact_projection::desktop_file_change_rows_preserve_runtime_path_evidence_fixture_passes()
            || !crate::desktop::artifact_projection::desktop_file_change_action_typed_projection_fixture_passes()
            || !crate::desktop::preferences::desktop_preferences_save_atomic_commit_fixture_passes()
            || !crate::app::session_title::app_session_title_fixture_domain_neutral_fixture_passes()
            || !desktop_turn_item_projection_sequence_fixture_passes()
            || !desktop_file_change_projection_sequence_fixture_passes())
    {
        diagnostics.push(
            "Desktop/TUI/history Markdown projection does not preserve canonical item ordering, fail-closed canonical history loading, chronological turn blocks, turn-local item sequence, call-id-scoped folded work/file-change evidence, and terminal outcome authority"
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

    if gate.gate_id
        == "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
        && !crate::agent::repair_lane::public_command_contract_failure_projects_compact_source_repair_fixture_passes()
    {
        diagnostics.push(
            "public-command repair lane fixture can still lose exact source target authority"
                .to_string(),
        );
    }
    if gate.gate_id
        == "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
    {
        let failed = [
            (
                "generated_test_contract_overreach_projects_test_repair",
                crate::agent::repair_lane::generated_test_contract_overreach_projects_test_repair_fixture_passes(),
            ),
            (
                "ungrounded_generated_public_output_assertion_projects_test_repair",
                crate::agent::repair_lane::ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes(),
            ),
            (
                "generated_test_public_output_numeric_format_overreach_projects_test_repair",
                crate::agent::repair_lane::generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes(),
            ),
            (
                "generated_test_exception_type_overreach_projects_test_repair",
                crate::agent::repair_lane::generated_test_exception_type_overreach_projects_test_repair_fixture_passes(),
            ),
            (
                "generic_generated_test_only_repair_lane_preserves_active_test_target",
                crate::agent::repair_lane::generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes(),
            ),
            (
                "contract_visible_public_exception_projects_source_repair",
                crate::agent::repair_lane::contract_visible_public_exception_projects_source_repair_fixture_passes(),
            ),
            (
                "source_only_contract_profile_no_synthetic_generated_test_target",
                crate::agent::contract_reconciliation::contract_reconciliation_source_only_profile_does_not_synthesize_generated_test_target_fixture_passes(),
            ),
            (
                "contract_reconciliation_target_identity_exact",
                crate::agent::contract_reconciliation::contract_reconciliation_preserves_workspace_relative_target_identity_fixture_passes(),
            ),
            (
                "contract_reconciliation_cluster_refs_exact_target_identity",
                crate::agent::contract_reconciliation::contract_reconciliation_cluster_refs_exact_target_identity_fixture_passes(),
            ),
            (
                "generated_test_local_binding_enrichment_exact_target_identity",
                crate::agent::state::generated_test_local_binding_enrichment_exact_target_identity_fixture_passes(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, passed)| (!passed).then_some(name))
        .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "public-command/generated-overreach repair lane fixtures can still use stale source/test reconciliation authority; failed fixtures: {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.state_reducer.requested_work_completion_promotes_verification" {
        let checks = [
            (
                "requested_work_completion_promotes_verification",
                crate::agent::state::requested_work_completion_promotes_verification_fixture_passes(
                ),
            ),
            (
                "requested_work_absolute_docs_file_change_promotes_verification",
                crate::agent::state::requested_work_absolute_docs_file_change_promotes_verification_fixture_passes(),
            ),
            (
                "requested_work_repair_continuation_expected_artifacts_do_not_reopen",
                crate::agent::state::requested_work_repair_continuation_expected_artifacts_do_not_reopen_fixture_passes(),
            ),
            (
                "required_verification_survives_authoring_completion",
                crate::agent::state::required_verification_survives_authoring_completion_fixture_passes(),
            ),
            (
                "state_authority_projection_uses_single_requested_work_owner",
                crate::agent::state::state_authority_projection_uses_single_requested_work_owner_fixture_passes(),
            ),
            (
                "state_authority_projection_replaces_stale_blocked_reason",
                crate::agent::state::state_authority_projection_replaces_stale_blocked_reason_fixture_passes(),
            ),
            (
                "partial_requested_work_remains_authoring_phase",
                crate::agent::state::partial_requested_work_remains_authoring_phase_fixture_passes(),
            ),
            (
                "state_requested_work_fixture_workflow_neutral",
                crate::agent::state::state_requested_work_fixtures_are_workflow_neutral_fixture_passes(),
            ),
            (
                "state_residual_component_fixture_workflow_neutral",
                crate::agent::state::state_residual_component_fixture_workflow_neutral_fixture_passes(),
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
                "state_new_authoring_turn_fixture_invariant_workspace_key",
                crate::agent::state::state_new_authoring_turn_fixture_invariant_workspace_key_fixture_passes(),
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
                "invalid_authoring_edit_no_progress_preserves_missing_requested_target",
                crate::agent::state::invalid_authoring_edit_no_progress_preserves_missing_requested_target_fixture_passes(),
            ),
            (
                "empty_artifact_tool_output_does_not_satisfy_requested_work",
                crate::agent::state::empty_artifact_tool_output_does_not_satisfy_requested_work_fixture_passes(),
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
                "metadata_only_tool_output_does_not_satisfy_file_change_authority",
                crate::agent::state::metadata_only_tool_output_does_not_satisfy_file_change_authority_fixture_passes(),
            ),
            (
                "state_history_item_sequence_primary_order",
                crate::agent::state::state_history_item_sequence_primary_order_fixture_passes(),
            ),
            (
                "state_handoff_remaining_exact_target_identity",
                crate::agent::state::state_handoff_remaining_exact_target_identity_fixture_passes(),
            ),
            (
                "state_blocked_reason_exact_target_identity",
                crate::agent::state::state_blocked_reason_exact_target_identity_fixture_passes(),
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
            (
                "state_structured_document_output_progress_exact_target_identity",
                crate::agent::state::structured_document_output_progress_exact_target_identity_fixture_passes(),
            ),
            (
                "state_structured_document_docling_progress_exact_target_identity",
                crate::agent::state::structured_document_docling_progress_exact_target_identity_fixture_passes(),
            ),
            (
                "state_structured_document_summary_generated_dependency_exclusion",
                crate::agent::state::structured_document_summary_skips_generated_dependency_targets_fixture_passes(),
            ),
            (
                "dotted_technology_token_not_file_target",
                crate::agent::completion_guard::completion_guard_does_not_treat_dotted_technology_token_as_file_target_fixture_passes(),
            ),
            (
                "todo_completion_kind_only_open_work_authority",
                crate::session::todo::todo_completion_kind_only_open_work_authority_fixture_passes(),
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

    if gate.gate_id == "preflight.state_reducer.post_repair_edit_promotes_verification_rerun" {
        let checks = [
            (
                "post_repair_file_change_promotes_verification_rerun",
                crate::agent::state::post_repair_file_change_promotes_verification_rerun_fixture_passes(),
            ),
            (
                "post_failure_runner_byproduct_filechange_does_not_satisfy_repair_progress",
                crate::agent::state::post_failure_runner_byproduct_filechange_does_not_satisfy_repair_progress_fixture_passes(),
            ),
            (
                "post_repair_edit_progress_promotes_shell_rerun",
                crate::agent::turn_decision::post_repair_edit_progress_promotes_shell_rerun_fixture_passes(),
            ),
            (
                "post_repair_verify_phase_ignores_stale_unclassified_repair",
                crate::agent::turn_decision::post_repair_verify_phase_ignores_stale_unclassified_repair_fixture_passes(),
            ),
            (
                "post_repair_required_verification_dispatch_is_runtime_owned",
                crate::agent::loop_impl::post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "successful repair edit progress can still leave stale repair edit authority instead of exact verification rerun; failed fixtures: {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.tool_lifecycle.verification_stable_tool_surface"
        && !crate::agent::repair_lane::repair_lane_typed_target_projection_no_required_action_shim_fixture_passes(
        )
    {
        diagnostics.push(
            "repair lane target projection can still retain stale required-action target outrank shims instead of relying on typed verification evidence, contract reconciliation, and exact target templates".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.verification_stable_tool_surface"
        && !crate::agent::turn_decision::turn_decision_repair_target_exact_path_authority_fixture_passes()
    {
        diagnostics.push(
            "turn decision repair-target diagnostics can still treat basename-only matches as active-work target authority instead of exact normalized path identity".to_string(),
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

    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_typed_tool_output_feedback_fixture_passes()
    {
        diagnostics.push(
            "prompt projection can still derive current recovery authority from legacy transcript titles instead of typed ToolOutput feedback metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::runtime_input_view_no_compatibility_transcript_authority_fixture_passes()
    {
        diagnostics.push(
            "RuntimeInputView can still expose compatibility Transcript materialization or cloned SessionRecord authority instead of accepting only canonical HistoryItem runtime input".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::provider_replay_sanitizes_content_shape_mismatch_from_typed_metadata_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still rely on ToolOutput title text instead of typed content-shape feedback metadata when sanitizing rejected write payloads".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::content_shape_contract::required_write_content_shape_mismatch_progress_class_fixture_passes()
    {
        diagnostics.push(
            "required-write content-shape mismatch ToolOutput can still split typed mismatch kind from operation_progress_class instead of reserving progress_effect for no-progress semantics".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::content_shape_contract::content_shape_contract_fixtures_are_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "content-shape contract fixtures can still use component/test_component/component-design surfaces instead of workflow-neutral source/test/docs artifact roles".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_artifact_target_kind_uses_language_adapter_fixture_passes()
    {
        diagnostics.push(
            "prompt artifact target classification can still bypass LanguageEvidenceAdapter roles with a local implementation extension allowlist".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::language_evidence::language_evidence_fixtures_are_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "LanguageEvidenceAdapter fixtures can still use component/widget/calculator domain surfaces instead of workflow-neutral source/test/docs artifact roles".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::loop_impl::loop_impl_escaped_source_fixture_language_neutral_fixture_passes(
        )
    {
        diagnostics.push(
            "TurnRuntime escaped-source recovery fixture can still prove a generic source target with Python-shaped payload grammar instead of target-language-consistent workflow source content".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && (!crate::agent::prompt_assets::prompt_assets_fixtures_are_workflow_neutral_fixture_passes(
        ) || !crate::agent::prompt_assets::staged_docs_deliverable_projection_workflow_neutral_fixture_passes()
            || !crate::agent::prompt_assets::documentation_target_classifier_shape_based_fixture_passes())
    {
        diagnostics.push(
            "Prompt-assets fixtures can still use component/widget domain surfaces, historical staged-doc target names, exact documentation filename branches, or raw verification summary classifiers instead of workflow-neutral prompt authority, deliverable-role projection, shape-based documentation target classification, and typed verification evidence projection".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_fixtures_are_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "Prompt replay fixtures can still use component/test_component/component-design domain surfaces instead of workflow-neutral source/test/docs provider replay authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_residual_fixtures_are_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "Prompt residual fixtures can still use component/manual-ST/Python game surfaces instead of workflow-neutral source/test/docs prompt lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_fixture_domain_neutral_fixture_passes()
    {
        diagnostics.push(
            "Prompt provider projection fixtures can still use route-specific vision, widget, or Python artifact surfaces instead of workflow-neutral image/source targets".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_verification_repair_fixture_language_neutral_fixture_passes(
        )
    {
        diagnostics.push(
            "Prompt verification repair and rejection fixtures can still use Python/component artifact surfaces instead of workflow-neutral source/test/docs prompt lifecycle authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_content_shape_window_fixture_workflow_neutral_fixture_passes(
        )
    {
        diagnostics.push(
            "Prompt content-shape history-window fixtures can still use component target coordinates instead of workflow-neutral test artifact authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_docs_followup_heuristic_domain_neutral_fixture_passes()
    {
        diagnostics.push(
            "Prompt docs-follow-up runtime heuristics can still use calculator or component domain wording instead of domain-neutral specification/capability-change intent".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::verification_repair_prompt_uses_language_projection_fixture_passes(
        )
    {
        diagnostics.push(
            "verification repair prompt projection can still leak Python/manual-ST specific guidance into generic code repair turns instead of using language evidence context".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_typed_verification_run_cycle_fixture_passes(
        )
    {
        diagnostics.push(
            "prompt projection can still derive verification repair cycle or failure-label authority from ToolResult title/summary text instead of typed verification_run metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::message_only_history_does_not_recreate_tool_lifecycle_prompt_state_fixture_passes(
        )
    {
        diagnostics.push(
            "message-only transcript prose can still recreate prompt lifecycle state without typed HistoryItem ToolOutput or state snapshot evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::verification_repair_read_budget_exhaustion_uses_typed_history_item_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "verification repair read-budget and focus-target prompt state can still be recreated from compatibility ToolResult title/summary text instead of typed HistoryItem ToolOutput metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::verification_repair_target_rotation_uses_typed_history_item_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "verification repair target-rotation prompt state can still be recreated from compatibility ToolResult title/summary text instead of typed HistoryItem ToolOutput metadata and canonical FileChange / verification_run history".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::verification_evidence_uses_typed_history_item_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "verification requirement satisfaction and pending clearance can still be derived from compatibility ToolResult title/summary text instead of typed HistoryItem verification_run evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::staged_task_closeout_repair_targets_use_typed_history_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "staged-task closeout repair targets can still be derived from compatibility unavailable-tool ToolResult title/summary text instead of typed HistoryItem feedback".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::staged_task_recovery_stall_uses_typed_history_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "staged-task recovery stall can still be derived from compatibility ToolResultPart success/progress fields instead of typed HistoryItem feedback".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::staged_task_output_lifecycle_uses_typed_history_authority_fixture_passes()
    {
        diagnostics.push(
            "staged-task output lifecycle can still be derived from compatibility ToolCall/ToolResult/DiffSummary projections instead of canonical HistoryItem ToolCall/ToolOutput/FileChange authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::documentation_prompt_lifecycle_uses_typed_history_authority_fixture_passes()
    {
        diagnostics.push(
            "documentation prompt lifecycle can still derive scope/evidence/stall state from compatibility transcript projections instead of canonical HistoryItem ToolCall/ToolOutput/FileChange authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::follow_up_focus_uses_typed_history_authority_fixture_passes()
    {
        diagnostics.push(
            "follow-up boundary, focus, and implicit documentation scope can still derive authority from compatibility transcript projections instead of canonical HistoryItem user/editor/tool/file-change evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::code_block_stall_uses_typed_history_authority_fixture_passes()
    {
        diagnostics.push(
            "code-block final-text drift recovery can still derive stall authority from compatibility assistant transcript text instead of typed rejected final-message or completion-drift evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::superseded_tool_denial_uses_typed_history_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "superseded unavailable-tool prompt guidance can still derive reminder authority from compatibility ToolResult title/summary text instead of typed RejectedToolProposal or ToolOutput metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::compaction_replay_uses_typed_history_authority_fixture_passes()
    {
        diagnostics.push(
            "compaction replay guidance can still derive continuity authority from compatibility assistant summary metadata instead of canonical HistoryItem compaction evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_workspace_root_uses_typed_runtime_input_fixture_passes()
    {
        diagnostics.push(
            "prompt projection workspace-root authority can still come from compatibility Transcript.session instead of typed runtime/session input".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::requested_work_parser_does_not_use_manual_st_harness_marker_fixture_passes()
    {
        diagnostics.push(
            "active requested-work parsing can still use manual ST / harness wording as an authority marker instead of a scenario-neutral typed continuation contract".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::requested_work_parser_does_not_use_case_stage_or_harness_owned_markers_fixture_passes()
    {
        diagnostics.push(
            "active requested-work parsing can still use route stage, attempt, or harness-owned wording as section/reference authority instead of typed section roles".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_staged_task_target_identity_exact_fixture_passes()
    {
        diagnostics.push(
            "prompt staged-task output target matching can still accept a foreign absolute or sibling-root suffix collision instead of exact workspace-relative target identity".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::provider_replay_inactive_filechange_exact_target_identity_fixture_passes()
    {
        diagnostics.push(
            "prompt provider replay inactive FileChange projection can still accept sibling-root or foreign absolute suffix collisions instead of exact normalized target identity".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt_assets::python_context_uses_language_evidence_adapter_fixture_passes(
        )
    {
        diagnostics.push(
            "prompt verification repair guidance can still infer Python context from raw failure-summary substrings instead of adapter-owned artifact language evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.verification.typed_evidence_cluster_authority"
        && !crate::agent::verification::verification_repair_cycle_uses_canonical_history_order_fixture_passes()
    {
        diagnostics.push(
            "verification repair cycle can still use raw input slice order instead of canonical HistoryItem order".to_string(),
        );
    }
    if gate.gate_id == "preflight.verification.typed_evidence_cluster_authority"
        && !crate::agent::verification::verification_history_sequence_primary_order_fixture_passes()
    {
        diagnostics.push(
            "verification evidence reconstruction can still use wall-clock timestamps as primary HistoryItem order instead of canonical sequence order".to_string(),
        );
    }
    if gate.gate_id == "preflight.verification.typed_evidence_cluster_authority"
        && !crate::agent::verification::verification_repair_cycle_history_item_authority_fixture_passes(
        )
    {
        diagnostics.push(
            "verification repair-cycle reconstruction can still use compatibility Transcript projection instead of canonical HistoryItem authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !crate::harness::manual_st::expected_artifacts_are_spec_owned_fixture_passes()
    {
        diagnostics.push(
            "manual ST expected artifact authority can still be injected by a case-specific harness fallback instead of explicit scenario spec sections".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_rejected_tool_proposal_fixture_passes()
    {
        diagnostics.push(
            "prompt projection can still derive invalid-tool stall authority from legacy ToolResult wording instead of typed RejectedToolProposal items".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_typed_pseudo_tool_rejection_fixture_passes(
        )
    {
        diagnostics.push(
            "prompt projection can still derive pseudo-tool stall authority from assistant transcript text instead of typed rejected final-message items".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_typed_docs_audit_metadata_fixture_passes()
    {
        diagnostics.push(
            "prompt projection can still derive staged documentation audit repair authority from ToolResult titles/summaries instead of typed docs reconciliation metadata".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.stale_write_arguments_summary_projection"
        && !crate::agent::prompt::prompt_projection_uses_state_patch_recovery_fixture_passes()
    {
        diagnostics.push(
            "prompt projection can still derive patch recovery authority from legacy ToolResult wording instead of typed failure state".to_string(),
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

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::loop_impl::provider_replay_effective_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still include executable historical tool calls or tool outputs that are outside the current effective tool surface".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::loop_impl::loop_impl_provider_replay_effective_surface_fixture_effective_test_payload_fixture_passes()
    {
        diagnostics.push(
            "provider replay effective-surface fixture can still preserve a placeholder accepted generated-test write payload instead of effective workflow test contract evidence".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::prompt::exact_write_repair_does_not_consume_untyped_read_as_supporting_context_replay()
    {
        diagnostics.push(
            "provider replay can still treat untyped successful read/list output as consumed supporting-context lifecycle evidence instead of requiring metadata-backed supporting_context authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::prompt::content_shape_mismatch_replay_requires_typed_metadata()
    {
        diagnostics.push(
            "provider replay can still classify untyped ToolOutput title text as content-shape lifecycle authority instead of requiring metadata-backed content-shape evidence".to_string(),
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
        && !crate::agent::loop_impl::provider_replay_omits_assistant_tool_call_content_fixture_passes()
    {
        diagnostics.push(
            "provider replay can still expose assistant tool-call content prose as completion authority while obligations remain open".to_string(),
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
        && !crate::agent::lifecycle_kernel::provider_surface_filter_requires_typed_supporting_context_signal_fixture_passes()
    {
        diagnostics.push(
            "effective-surface provider replay can still classify plain natural-language grounding/context prose as supporting-context lifecycle evidence instead of requiring a typed ToolFeedbackEnvelope signal".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::lifecycle_kernel::provider_surface_filter_requires_typed_provider_noncompliance_signal_fixture_passes()
    {
        diagnostics.push(
            "effective-surface provider replay can still classify plain natural-language provider-noncompliance prose as corrective lifecycle evidence instead of requiring a typed ToolFeedbackEnvelope signal".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::lifecycle_kernel::provider_surface_filter_rejects_spoofed_tool_feedback_text_fixture_passes()
    {
        diagnostics.push(
            "effective-surface provider replay can still classify spoofed `[tool feedback]` text inside a ToolOutput result as lifecycle authority instead of requiring metadata-backed typed feedback".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.tool_pair_symmetry"
        && !crate::agent::lifecycle_kernel::lifecycle_kernel_fixtures_are_workflow_neutral_fixture_passes()
    {
        diagnostics.push(
            "lifecycle kernel provider replay and adjudication fixtures can still use language-shaped payload content as generic workflow source authority".to_string(),
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
        && !crate::agent::prompt::invalid_edit_arguments_replay_requires_typed_metadata()
    {
        diagnostics.push(
            "provider replay can still classify untyped ToolOutput title text as invalid edit arguments lifecycle authority instead of requiring metadata-backed invalid_edit_arguments evidence".to_string(),
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
        && !crate::agent::prompt::compaction_provider_context_projects_typed_contract_before_summary(
        )
    {
        diagnostics.push(
            "provider replay can still project compaction summary prose before the typed continuation contract".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::prompt::content_shape_repair_contract_uses_canonical_history_window_fixture_passes()
    {
        diagnostics.push(
            "content-shape repair prompt projection can still scan pre-compaction ToolOutput feedback by using display transcript window indices instead of canonical history item order".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::prompt::provider_replay_compaction_boundary_uses_canonical_history_order_fixture_passes()
    {
        diagnostics.push(
            "provider replay compaction boundary can still use raw input slice order instead of canonical HistoryItem order".to_string(),
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
        && !crate::agent::compaction::compaction_trigger_uses_canonical_history_order_fixture_passes(
        )
    {
        diagnostics.push(
            "compaction trigger can still count pre-summary history by raw input slice order instead of canonical HistoryItem order".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::llm_summary_text_is_wrapped_with_typed_continuity_fixture_passes()
    {
        diagnostics.push(
            "model-returned compaction summary text can still omit the typed CompactionContinuity marker or continuation focus".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_summary_ignores_model_claimed_continuity_fixture_passes()
    {
        diagnostics.push(
            "model-returned compaction summary text can still satisfy continuity by mentioning marker strings instead of receiving the runtime-owned typed continuation block".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_continuity_carries_lifecycle_guard_snapshot_fixture_passes()
    {
        diagnostics.push(
            "compaction continuity can still replace lifecycle guard history without carrying the latest guard snapshot refs and payload".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_continuity_uses_canonical_history_order_for_lifecycle_guard_snapshot_fixture_passes()
    {
        diagnostics.push(
            "compaction continuity can still select LifecycleGuardSnapshot from raw input slice order instead of canonical HistoryItem order".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_sequence_order_resists_timestamp_drift_fixture_passes()
    {
        diagnostics.push(
            "compaction trigger can still use wall-clock timestamps as primary history item order instead of canonical sequence order".to_string(),
        );
    }
    if gate.gate_id == "preflight.prompt_replay.compaction_orphan_assistant_repaired"
        && !crate::agent::compaction::compaction_lifecycle_guard_sequence_order_resists_timestamp_drift_fixture_passes()
    {
        diagnostics.push(
            "compaction continuity can still select LifecycleGuardSnapshot by timestamp drift instead of canonical sequence order".to_string(),
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
        && !crate::agent::prompt::metadata_only_tool_output_does_not_create_filechange_reference_snapshot()
    {
        diagnostics.push(
            "provider replay can still create inactive FileChange reference snapshots from ToolOutput metadata without canonical FileChange item authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::agent::prompt::failed_inactive_authoring_feedback_requires_typed_metadata()
    {
        diagnostics.push(
            "provider replay can still classify untyped ToolOutput title text as failed wrong-target lifecycle feedback instead of requiring metadata-backed wrong_authoring_target authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::edit::successful_file_change_tool_feedback_is_evidence_only()
    {
        diagnostics.push(
            "successful FileChange ToolResult feedback can still project completed artifact edit operations instead of remaining evidence-only under TurnControlEnvelope authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.prompt_replay.stale_inactive_authoring_pair_omitted"
        && !crate::agent::tool_orchestrator::wrong_authoring_target_feedback_projects_current_action_fixture_passes()
    {
        diagnostics.push(
            "wrong-target corrective ToolOutput feedback can still omit the current required action and operation template while naming the stale submitted target".to_string(),
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
            "provider replay can still expose stale progress-projection todo JSON as executable assistant tool-call arguments, omit typed current call-id-scoped progress feedback, or treat untyped progress-projection result text as current feedback authority".to_string(),
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
        && !crate::llm::contract::provider_policy_tool_lifecycle_upgrade_fixture_passes()
    {
        diagnostics.push(
            "OpenAI-compatible-only provider policy can still treat an existing base policy prefix as complete lifecycle projection and omit tool lifecycle authority on tool-enabled re-render".to_string(),
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
        && !crate::llm::contract::llm_contract_current_provider_profile_fixture_passes()
    {
        diagnostics.push(
            "llm contract ChatRequest fixtures can still retain stale local provider profile authority instead of the current closed-network LM Studio provider profile".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::model_probe::model_probe_rejects_extra_tool_arguments_fixture_passes()
    {
        diagnostics.push(
            "model availability tool-call probe can still pass schema-violating function.arguments with extra properties instead of enforcing typed argument schema evidence".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::model_tool_replay_metadata_is_not_serialized_fixture_passes()
    {
        diagnostics.push(
            "replay-only ToolOutput metadata can still serialize through the provider-facing ModelMessage contract instead of staying local to replay normalization".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::model_tool_replay_metadata_is_not_deserialized_fixture_passes()
    {
        diagnostics.push(
            "replay-only ToolOutput metadata can still be deserialized from provider-facing ModelMessage JSON instead of being sourced only from canonical ToolOutput items".to_string(),
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
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::provider_extra_body_cannot_override_runtime_request_fields_fixture_passes()
    {
        diagnostics.push(
            "provider extra_body_json can still override runtime-owned request fields such as tool_choice, tools, messages, model, or output budget after TurnControlEnvelope validation".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::provider_extra_body_cannot_override_parallel_tool_calls_fixture_passes()
    {
        diagnostics.push(
            "provider extra_body_json can still own or override parallel_tool_calls instead of receiving it from typed ChatRequest tool lifecycle state".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::provider_payload_preserves_parallel_tool_calls_false_fixture_passes()
    {
        diagnostics.push(
            "OpenAI-compatible payload can still omit explicit parallel_tool_calls=false and fall back to provider default concurrency behavior".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::provider_payload_omits_parallel_tool_calls_without_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "OpenAI-compatible no-tool payload can still project parallel_tool_calls as a tool-lifecycle concurrency field or accept provider extra_body concurrency authority".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::effective_parallel_tool_calls_requires_tool_surface_and_prediction_capacity_fixture_passes()
    {
        diagnostics.push(
            "effective parallel tool-call projection can still use raw config flag without requiring non-empty tool surface and max_parallel_predictions greater than one".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::control_plane_parallel_tool_calls_matches_effective_projection_fixture_passes()
    {
        diagnostics.push(
            "TurnContext control-plane parallel_tool_calls projection can still diverge from the effective ChatRequest/provider payload predicate".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::model_probe::model_availability_probe_uses_shared_transport_projection_fixture_passes()
    {
        diagnostics.push(
            "model availability tool-call probe can still hand-roll provider transport fields instead of using the same typed tool_choice and parallel tool-call projection as runtime dispatch".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::config::model::config_default_provider_profile_lm_studio_fixture_passes()
    {
        diagnostics.push(
            "default config bootstrap can still create stale provider settings instead of the current closed-network LM Studio provider profile".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::config::model::provider_metadata_mode_default_lm_studio_fixture_passes()
    {
        diagnostics.push(
            "ProviderMetadataMode::default can still restore stale OpenAI-compatible/vLLM mode instead of the current LM Studio native-required provider authority".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::model_probe::model_availability_probe_pass_hydrates_runtime_tool_capability_fixture_passes()
    {
        diagnostics.push(
            "model availability tool-call probe pass can still be accepted by the gate without hydrating the runtime model profile as tool-capable".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::model_probe::model_availability_vision_probe_hydrates_runtime_image_capability_fixture_passes()
    {
        diagnostics.push(
            "model availability vision-required gate can still rely on provider metadata without hydrating runtime image capability from actual image probe evidence".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::app::run_service::runtime_model_hydration_uses_availability_probe_evidence_fixture_passes()
    {
        diagnostics.push(
            "runtime model hydration can still bypass model availability probe evidence and build ModelProfile from metadata-only capability state".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::tui::prompt_enhance::prompt_enhance_model_preparation_uses_availability_report_fixture_passes()
    {
        diagnostics.push(
            "prompt enhance can still bypass model availability probe evidence and build ModelProfile from raw config/list-only availability".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::desktop::startup::DesktopStartupState::desktop_startup_uses_model_availability_report_fixture_passes()
    {
        diagnostics.push(
            "Desktop startup readiness can still treat provider catalog model presence as configured model availability without the tool-call probe gate".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && (!crate::desktop::app::desktop_image_dispatch_delegates_capability_to_runtime_fixture_passes()
            || !crate::desktop::web_model::desktop_image_input_delegates_capability_to_runtime_fixture_passes())
    {
        diagnostics.push(
            "Desktop image input or dispatch can still fail-close from stale effective_config.supports_images before RunService model availability runs".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::desktop::web_model::desktop_model_summary_marks_capability_metadata_not_runtime_authority_fixture_passes()
    {
        diagnostics.push(
            "Desktop provider model summary can still project provider metadata false as a runtime tool/image support verdict".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::chat_request_tool_lifecycle_fields_match_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "ChatRequest lifecycle fields can still diverge from the effective tool surface before OpenAI-compatible provider serialization".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::chat_request_image_parts_require_vision_capability_fixture_passes()
    {
        diagnostics.push(
            "ChatRequest image content parts can still reach OpenAI-compatible provider serialization without model vision capability authority".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::openai_compat::chat_request_tools_require_tool_capability_fixture_passes()
    {
        diagnostics.push(
            "ChatRequest provider tool schemas can still reach OpenAI-compatible serialization without model tool capability authority".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::agent::loop_impl::request_diagnostics_tool_choice_uses_runtime_dispatch_field_fixture_passes()
    {
        diagnostics.push(
            "request diagnostics can still report tool_choice from provider extra_body_json instead of the runtime-owned dispatch field".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::agent::loop_impl::request_diagnostics_tool_surface_uses_chat_request_fixture_passes()
    {
        diagnostics.push(
            "request diagnostics can still report tool_count, tool_names, or tool_schemas from a stale caller-local tool vector instead of the ChatRequest provider payload".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::agent::loop_impl::request_diagnostics_model_capabilities_use_chat_request_fixture_passes()
    {
        diagnostics.push(
            "request diagnostics can still omit the model capability snapshot used by ChatRequest provider lifecycle validation".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::agent::loop_impl::request_diagnostics_missing_model_capabilities_remain_absent_fixture_passes()
    {
        diagnostics.push(
            "request diagnostics can still synthesize false model capability authority when stored diagnostics artifacts omit the capability snapshot".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::agent::loop_impl::request_diagnostics_parallel_tool_calls_scope_matches_chat_request_fixture_passes()
    {
        diagnostics.push(
            "request diagnostics can still omit explicit parallel_tool_calls=false for tool-bearing ChatRequests or synthesize concurrency evidence for no-tool requests".to_string(),
        );
    }
    if gate.gate_id
        == "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
        && !crate::llm::contract::chat_request_tool_choice_is_provider_neutral_typed_fixture_passes(
        )
    {
        diagnostics.push(
            "ChatRequest tool_choice can still be carried as provider-shaped raw JSON instead of a provider-neutral typed dispatch projection".to_string(),
        );
    }

    if gate.gate_id == "preflight.lifecycle_kernel.provider_noncompliance_adjudication"
        && !crate::agent::lifecycle_kernel::provider_noncompliance_adjudication_fixture_passes()
    {
        diagnostics.push(
            "schema-outside or malformed provider tool proposals are not adjudicated into provider_noncompliance lifecycle evidence with shared ToolResult feedback, rejected proposal metadata, and semantic no-progress hash".to_string(),
        );
    }
    if gate.gate_id == "preflight.lifecycle_kernel.turn_lifecycle_plan_authority" {
        let fixture_results = [
            (
                crate::agent::lifecycle_kernel::turn_lifecycle_plan_owns_dispatch_tool_choice_fixture_passes(),
                "turn_lifecycle_plan_owns_dispatch_tool_choice",
            ),
            (
                crate::agent::lifecycle_kernel::provider_noncompliance_recovery_overrides_grounding_fixture_passes(),
                "provider_noncompliance_recovery_overrides_grounding",
            ),
            (
                crate::agent::lifecycle_kernel::wrong_target_authoring_recovery_hardens_active_target_fixture_passes(),
                "wrong_target_authoring_recovery_hardens_active_target",
            ),
            (
                crate::agent::lifecycle_kernel::edit_only_authoring_grounding_overrides_repair_grounding_fixture_passes(),
                "edit_only_authoring_grounding_overrides_repair_grounding",
            ),
            (
                crate::agent::prompt_assets::inactive_target_edit_recovery_reminder_uses_current_edit_surface_fixture_passes(),
                "inactive_target_edit_recovery_reminder_uses_current_edit_surface",
            ),
            (
                crate::agent::lifecycle_kernel::malformed_apply_patch_recovery_overrides_stale_wrong_target_fixture_passes(),
                "malformed_apply_patch_recovery_overrides_stale_wrong_target",
            ),
            (
                crate::agent::loop_impl::turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields_fixture_passes(),
                "turn_runtime_lifecycle_guard_state_owns_mutable_guard_fields",
            ),
            (
                crate::agent::loop_impl::lifecycle_guard_snapshot_hydrates_runtime_state_fixture_passes(),
                "lifecycle_guard_snapshot_hydrates_runtime_state",
            ),
            (
                crate::agent::loop_impl::lifecycle_guard_snapshot_hydration_uses_canonical_item_order_fixture_passes(),
                "lifecycle_guard_snapshot_hydration_uses_canonical_item_order",
            ),
            (
                crate::agent::loop_impl::lifecycle_guard_snapshot_hydration_sequence_order_resists_timestamp_drift_fixture_passes(),
                "loop_impl_lifecycle_guard_hydration_sequence_order",
            ),
            (
                crate::agent::loop_impl::control_envelope_preserves_current_turn_id_fixture_passes(),
                "loop_impl_control_envelope_current_turn_id",
            ),
        ];
        let failed_fixture_ids = fixture_results
            .iter()
            .filter_map(|(passed, fixture_id)| (!passed).then_some(*fixture_id))
            .collect::<Vec<_>>();
        if !failed_fixture_ids.is_empty() {
            diagnostics.push(format!(
                "typed lifecycle authority fixture(s) failed: {}",
                failed_fixture_ids.join(", ")
            ));
            diagnostics.push(
                "dispatch tool_choice, replay policy, proposal policy, corrective policy, terminal policy, continuation expectation, diagnostics projection, or lifecycle guard state is still owned by loose TurnRuntime branch policy instead of typed lifecycle owner structures".to_string(),
            );
        }
    }
    if gate.gate_id == "preflight.route_evidence.schema"
        && !artifact_replay_rejects_empty_route_evidence_fixture_passes()
    {
        diagnostics.push(
            "artifact replay preflight can still pass empty or fabricated route evidence instead of validating typed route artifact schema content".to_string(),
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
    if gate.gate_id == "preflight.tool_lifecycle.synthetic_feedback_not_verification_authority"
        && !crate::tool::truncate::truncated_tool_output_feedback_uses_typed_tool_surface_fixture_passes()
    {
        diagnostics.push(
            "truncated ToolOutput follow-up guidance can still recommend an unavailable tool surface instead of registered read/grep recovery tools".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.synthetic_feedback_not_verification_authority"
        && (!crate::harness::runtime_writer::no_progress_signature_projection_matches_schema_fixture_passes()
            || !crate::harness::schema::tool_no_progress_signature_schema_matches_runtime_projection_fixture_passes())
    {
        diagnostics.push(
            "harness no-progress signature runtime projection and exported schema can still drift, leaving terminal guard evidence rejected or under-specified by the typed schema".to_string(),
        );
    }
    if gate.gate_id == "preflight.tool_lifecycle.command_text_encoding_contract"
        && !crate::tool::shell::shell_contract_violation_typed_no_progress_feedback_fixture_passes()
    {
        diagnostics.push(
            "pre-execution shell syntax or encoding corrections can still bypass typed no-progress ToolFeedbackEnvelope/result_hash projection".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.no_content_write_is_no_progress" {
        let failed_no_content_fixtures = [
            (
                "no_content_write_metadata_projects_no_progress",
                crate::agent::tool_orchestrator::no_content_write_metadata_projects_no_progress_fixture_passes(),
            ),
            (
                "no_content_write_result_projects_typed_no_progress",
                crate::tool::write::no_content_write_result_projects_typed_no_progress_fixture_passes(),
            ),
            (
                "write_no_content_fixture_language_neutral",
                crate::tool::write::no_content_write_fixture_is_language_neutral_fixture_passes(),
            ),
            (
                "write_text_file_commit_plan_avoids_pre_remove",
                crate::tool::write_support::write_text_file_commit_plan_avoids_pre_remove_fixture_passes(),
            ),
            (
                "write_execution_uses_atomic_filechange_commit",
                crate::tool::write::write_execution_uses_atomic_filechange_commit_fixture_passes(),
            ),
            (
                "no_content_apply_patch_projects_idempotent_no_progress",
                crate::tool::apply_patch::no_content_apply_patch_projects_idempotent_no_progress_fixture_passes(),
            ),
            (
                "no_content_apply_patch_metadata_projects_idempotent_no_progress",
                crate::agent::tool_orchestrator::no_content_apply_patch_metadata_projects_idempotent_no_progress_fixture_passes(),
            ),
            (
                "mixed_patch_noop_update_keeps_file_change_admission_progress_capable",
                crate::tool::apply_patch::mixed_patch_noop_update_keeps_file_change_admission_progress_capable_fixture_passes(),
            ),
            (
                "apply_patch_admission_covers_all_operation_kinds_before_side_effects",
                crate::tool::apply_patch::apply_patch_admission_covers_all_operation_kinds_before_side_effects_fixture_passes(),
            ),
            (
                "apply_patch_move_destination_requires_fresh_authority",
                crate::tool::apply_patch::apply_patch_move_destination_requires_fresh_authority_fixture_passes(),
            ),
            (
                "multi_path_edit_locks_are_deterministic",
                crate::edit::safety::multi_path_edit_locks_are_deterministic_fixture_passes(),
            ),
            (
                "apply_patch_permission_is_single_invocation_admission",
                crate::tool::apply_patch::apply_patch_permission_is_single_invocation_admission_fixture_passes(),
            ),
            (
                "apply_patch_permission_precedes_formatter_validation",
                crate::tool::apply_patch::apply_patch_permission_precedes_formatter_validation_fixture_passes(),
            ),
            (
                "apply_patch_validation_and_execution_share_tool_invocation_lock",
                crate::tool::apply_patch::apply_patch_validation_and_execution_share_tool_invocation_lock_fixture_passes(),
            ),
            (
                "apply_patch_validation_materializes_single_execution_plan",
                crate::tool::apply_patch::apply_patch_validation_materializes_single_execution_plan_fixture_passes(),
            ),
            (
                "apply_patch_admitted_plan_rejects_duplicate_participant",
                crate::tool::apply_patch::apply_patch_admitted_plan_rejects_duplicate_participant_fixture_passes(),
            ),
            (
                "apply_patch_duplicate_participant_rejected_before_formatter",
                crate::tool::apply_patch::apply_patch_duplicate_participant_rejected_before_formatter_fixture_passes(),
            ),
            (
                "apply_patch_admitted_execution_uses_atomic_commit_transaction",
                crate::tool::apply_patch::apply_patch_admitted_execution_uses_atomic_commit_transaction_fixture_passes(),
            ),
            (
                "docs_route_idempotent_write_no_progress_terminal_guard",
                crate::agent::loop_impl::docs_route_idempotent_write_no_progress_terminal_guard_fixture_passes(),
            ),
            (
                "empty_file_change_is_not_authoring_progress",
                crate::agent::tool_orchestrator::empty_file_change_is_not_authoring_progress_fixture_passes(),
            ),
            (
                "empty_artifact_no_progress_terminal_guard",
                crate::agent::tool_orchestrator::empty_artifact_no_progress_terminal_guard_fixture_passes(),
            ),
            (
                "corrective_content_shape_guard_rejects_untyped_no_progress",
                crate::agent::tool_orchestrator::corrective_content_shape_guard_rejects_untyped_no_progress_fixture_passes(),
            ),
            (
                "invalid_edit_arguments_project_no_progress_recovery",
                crate::agent::loop_impl::invalid_edit_arguments_project_no_progress_recovery_fixture_passes(),
            ),
            (
                "loop_impl_invalid_edit_fixture_language_neutral",
                crate::agent::loop_impl::loop_impl_invalid_edit_fixture_language_neutral_fixture_passes(),
            ),
            (
                "invalid_edit_arguments_recovery_is_system_control_projection",
                crate::agent::loop_impl::invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes(),
            ),
            (
                "invalid_edit_recovery_projects_candidate_target_operation",
                crate::agent::loop_impl::invalid_edit_recovery_projects_candidate_target_operation_fixture_passes(),
            ),
            (
                "invalid_edit_recovery_uses_open_target_when_candidate_is_inactive",
                crate::agent::edit_recovery::invalid_edit_recovery_uses_open_target_when_candidate_is_inactive_fixture_passes(),
            ),
            (
                "invalid_edit_recovery_candidate_target_normalized",
                crate::agent::edit_recovery::invalid_edit_recovery_candidate_target_normalized_fixture_passes(),
            ),
            (
                "mixed_target_apply_patch_preserves_active_hunk_evidence",
                crate::agent::edit_recovery::mixed_target_apply_patch_preserves_active_hunk_evidence_fixture_passes(),
            ),
            (
                "mixed_target_invalid_edit_recovery_projects_into_control_envelope",
                crate::agent::loop_impl::mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes(),
            ),
            (
                "mixed_target_invalid_edit_replay_is_target_exclusive",
                crate::agent::prompt::mixed_target_invalid_edit_replay_is_target_exclusive_fixture_passes(),
            ),
            (
                "inactive_target_content_shape_replay_is_target_exclusive",
                crate::agent::prompt::inactive_target_content_shape_replay_is_target_exclusive_fixture_passes(),
            ),
            (
                "content_shape_recovery_projection_omits_inactive_submitted_targets",
                crate::agent::loop_impl::content_shape_recovery_projection_omits_inactive_submitted_targets_fixture_passes(),
            ),
            (
                "apply_patch_context_mismatch_enters_invalid_edit_lifecycle",
                crate::agent::edit_recovery::apply_patch_context_mismatch_enters_invalid_edit_lifecycle_fixture_passes(),
            ),
            (
                "invalid_edit_arguments_recovery_persists_across_final_message",
                crate::agent::loop_impl::invalid_edit_arguments_recovery_persists_across_final_message_fixture_passes(),
            ),
            (
                "malformed_write_patch_capable_recovery_surface",
                crate::agent::loop_impl::malformed_write_patch_capable_recovery_surface_fixture_passes(),
            ),
            (
                "loop_impl_malformed_write_fixture_language_neutral",
                crate::agent::loop_impl::loop_impl_malformed_write_fixture_language_neutral_fixture_passes(),
            ),
            (
                "malformed_apply_patch_write_recovery_surface",
                crate::agent::loop_impl::malformed_apply_patch_write_recovery_surface_fixture_passes(),
            ),
            (
                "loop_impl_malformed_apply_patch_fixture_language_neutral",
                crate::agent::loop_impl::loop_impl_malformed_apply_patch_fixture_language_neutral_fixture_passes(),
            ),
            (
                "invalid_apply_patch_write_recovery_normalizes_active_targets",
                crate::agent::edit_recovery::invalid_apply_patch_write_recovery_normalizes_active_targets_fixture_passes(),
            ),
            (
                "edit_recovery_fixture_workflow_neutral",
                crate::agent::edit_recovery::edit_recovery_targets_and_fixtures_are_workflow_neutral_fixture_passes(),
            ),
            (
                "malformed_write_arguments_terminal_quote_repair",
                crate::agent::loop_impl::malformed_write_arguments_terminal_quote_repair_fixture_passes(),
            ),
            (
                "destructive_noop_patch_is_rejected",
                crate::tool::apply_patch::destructive_noop_patch_is_rejected_fixture_passes(),
            ),
            (
                "empty_or_zero_diff_patch_is_rejected",
                crate::tool::apply_patch::empty_or_zero_diff_patch_is_rejected_fixture_passes(),
            ),
            (
                "hunkless_update_patch_is_rejected",
                crate::tool::apply_patch::hunkless_update_patch_is_rejected_fixture_passes(),
            ),
            (
                "markdown_update_body_without_diff_prefix_is_rejected",
                crate::tool::apply_patch::markdown_update_body_without_diff_prefix_is_rejected_fixture_passes(),
            ),
            (
                "patch_context_matching_is_exact",
                crate::edit::patch::patch_context_matching_is_exact_fixture_passes(),
            ),
            (
                "add_file_unprefixed_content_line_feedback_names_line",
                crate::tool::apply_patch::add_file_unprefixed_content_line_feedback_names_line_fixture_passes(),
            ),
            (
                "edit_patch_parser_feedback_language_neutral",
                crate::edit::patch::edit_patch_parser_feedback_language_neutral_fixture_passes(),
            ),
            (
                "text_artifact_short_serialized_markdown_rejected",
                crate::agent::content_shape_contract::text_artifact_readable_shape_rejects_short_serialized_markdown_fixture_passes(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, passed)| (!passed).then_some(name))
        .collect::<Vec<_>>();
        if !failed_no_content_fixtures.is_empty() {
            diagnostics.push(
            format!(
                "no-content write output, idempotent docs writes, malformed edit argument feedback / patch-capable recovery surface, permissive patch context matching, or destructive no-op acknowledgement patch can still be projected without typed no-progress repair authority; failed fixtures: {}",
                failed_no_content_fixtures.join(", ")
            ),
        );
        }
    }

    if gate.gate_id == "preflight.tool_lifecycle.active_authoring_rejects_wrong_target" {
        let failed_active_authoring = [
            (
                "active_authoring_rejects_wrong_target",
                crate::agent::loop_impl::active_authoring_rejects_wrong_target_fixture_passes(),
            ),
            (
                "loop_impl_active_authoring_docs_regression_fixture_domain_neutral",
                crate::agent::loop_impl::loop_impl_active_authoring_docs_regression_fixture_domain_neutral_fixture_passes(),
            ),
            (
                "verification_repair_rejects_non_exact_write_target",
                crate::agent::loop_impl::verification_repair_rejects_non_exact_write_target_fixture_passes(),
            ),
            (
                "exact_write_wrong_path_content_shape_uses_active_target",
                crate::agent::tool_orchestrator::exact_write_wrong_path_content_shape_uses_active_target_fixture_passes(),
            ),
            (
                "exact_apply_patch_wrong_path_content_shape_uses_active_target",
                crate::agent::tool_orchestrator::exact_apply_patch_wrong_path_content_shape_uses_active_target_fixture_passes(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, passed)| (!passed).then_some(name))
        .collect::<Vec<_>>();
        if !failed_active_authoring.is_empty() {
            diagnostics.push(format!(
                "requested-work authoring or verification repair still accepts content-changing writes outside the active deliverable / exact repair target set as progress; failed fixtures: {}",
                failed_active_authoring.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.turn_decision.repair_required_active_work_ignores_shell_only_continuation"
        && !crate::agent::turn_decision::repair_required_active_work_rejects_shell_only_surface_fixture_passes()
    {
        diagnostics.push(
            "repair-required active work can still be projected as a shell-only verification rerun surface instead of failing closed on the missing edit surface".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.executed_failure_call_output_terminal_guard"
        && (!crate::agent::tool_orchestrator::executed_tool_failure_metadata_fixture_passes()
            || !crate::agent::loop_impl::executed_tool_failure_terminal_guard_fixture_passes()
            || !crate::agent::loop_impl::loop_impl_terminal_guard_fixture_language_neutral_fixture_passes()
            || !crate::agent::loop_impl::same_verification_failure_terminal_guard_fixture_passes())
    {
        diagnostics.push(
            "executed tool failures are not preserved as call-scoped failed outputs with stable no-progress terminal guard".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.verification_stable_tool_surface" {
        let checks = [
            (
                "verification_active_work_preserves_tool_surface_and_rejects_wrong_command",
                crate::agent::loop_impl::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_fixture_passes(),
            ),
            (
                "repair_active_shell_probe_uses_repair_target_authority",
                crate::agent::loop_impl::repair_active_shell_probe_uses_repair_target_authority_fixture_passes(),
            ),
            (
                "singleton_verification_command_arguments_are_runtime_owned",
                crate::agent::loop_impl::singleton_verification_command_arguments_are_runtime_owned_fixture_passes(),
            ),
            (
                "verification_only_missing_provider_tool_call_dispatches_runtime_owned",
                crate::agent::loop_impl::verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes(),
            ),
            (
                "public_verification_command_identity_dedupes_required_commands",
                crate::agent::state::public_verification_command_identity_dedupes_required_commands_fixture_passes(),
            ),
            (
                "verification_only_authority_narrows_to_exact_shell",
                crate::protocol::verification_only_authority_narrows_to_exact_shell_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            let active_work_failed_checks = crate::agent::loop_impl::verification_active_work_preserves_tool_surface_and_rejects_wrong_command_failed_checks();
            diagnostics.push(format!(
                "verification active work does not project exact shell verification authority, or repair-required verification lost its edit-capable recovery surface; failed fixtures: {}",
                failed.join(", ")
            ));
            if !active_work_failed_checks.is_empty() {
                diagnostics.push(format!(
                    "verification_active_work_preserves_tool_surface_and_rejects_wrong_command failed checks: {}",
                    active_work_failed_checks.join(", ")
                ));
            }
        }
    }

    if gate.gate_id == "preflight.tool_lifecycle.authoring_stable_tool_surface"
        && (!crate::agent::loop_impl::open_authoring_operation_intent_preserves_tool_surface_fixture_passes()
            || !crate::agent::loop_impl::loop_impl_operation_intent_fixture_language_neutral_fixture_passes()
            || !crate::agent::loop_impl::loop_impl_active_authoring_docs_regression_fixture_domain_neutral_fixture_passes())
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
                "docs_route_idempotent_write_no_progress_terminal_guard",
                crate::agent::loop_impl::docs_route_idempotent_write_no_progress_terminal_guard_fixture_passes(),
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
                "grounding_target_matching_rejects_foreign_suffix_collision",
                crate::agent::grounding_evidence::grounding_target_matching_rejects_foreign_suffix_collision_fixture_passes(),
            ),
            (
                "grounding_metadata_path_target_identity_exact",
                crate::agent::grounding_evidence::grounding_metadata_path_matching_rejects_foreign_suffix_collision_fixture_passes(),
            ),
            (
                "docs_route_grep_line_path_generic_path_line",
                crate::agent::grounding_evidence::docs_route_grep_line_path_generic_path_line_fixture_passes(),
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
                "verification_repair_supporting_context_loop_terminal_cluster",
                crate::agent::tool_orchestrator::verification_repair_supporting_context_converges_by_obligation_fixture_passes(),
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
                "loop_impl_docs_budget_edit_surface_fixture_language_neutral",
                crate::agent::loop_impl::loop_impl_docs_budget_edit_surface_fixture_language_neutral_fixture_passes(),
            ),
            (
                "loop_impl_docs_route_budget_fixture_workflow_neutral",
                crate::agent::loop_impl::docs_route_supporting_context_budget_fixture_workflow_neutral_fixture_passes(),
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
        && (!crate::agent::loop_impl::edit_surface_registry_symmetry_fixture_passes()
            || !crate::agent::loop_impl::loop_impl_docs_budget_edit_surface_fixture_language_neutral_fixture_passes())
    {
        diagnostics.push(
            "core edit tool surface and runtime dispatch registry can still diverge, or failed inactive write feedback is not preserved as a call-id-scoped ToolCall/ToolOutput pair".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"
        && (!crate::agent::tool_orchestrator::rejected_tool_semantic_terminal_guard_fixture_passes()
            || !crate::agent::loop_impl::non_edit_invalid_tool_arguments_terminal_guard_fixture_passes()
            || !crate::tool::registry::unknown_tool_feedback_does_not_restore_shell_surface_fixture_passes())
    {
        diagnostics.push(
            "rejected known-tool feedback still uses unstable argument/projection keys, fails to terminalize repeated disallowed or malformed proposals before outer timeout, or unknown-tool feedback can still restore a broad shell surface outside the active turn control envelope; provider noncompliance is verified by the lifecycle-kernel gate".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.workspace_relative_file_change_authority"
        && (!crate::workspace::project::path_separator_normalization_fixture_passes()
            || !crate::edit::change_path_storage_uses_workspace_relative_authority()
            || !crate::tool::apply_patch::apply_patch_file_change_storage_uses_workspace_relative_paths_fixture_passes()
            || !crate::tool::search::glob_workspace_relative_pattern_fixture_passes())
    {
        diagnostics.push(
            "file-change lifecycle still stores route-root, absolute, or separator-drifted paths instead of workspace-relative authority, or glob matching/output still uses absolute host paths as model-visible authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.shell_mutation_syncs_edit_baseline"
        && (!crate::edit::safety::shell_mutation_syncs_confirmed_edit_baseline_fixture_passes()
            || !crate::tool::shell::shell_change_set_syncs_confirmed_edit_baseline_fixture_passes()
            || !crate::tool::shell::shell_change_set_restores_baseline_on_persistence_failure_fixture_passes())
    {
        diagnostics.push(
            "shell-detected workspace file mutations can still bypass the confirmed-content baseline used by write/apply_patch stale-change guards, or can leave baseline-only hidden state when FileChange evidence persistence fails".to_string(),
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
    if gate.gate_id == "preflight.tool_lifecycle.shell_timeout_process_tree_authority"
        && !crate::harness::manual_st::route_owned_command_timeout_cleans_process_tree_fixture_passes()
    {
        diagnostics.push(
            "manual ST route-owned verification timeout must cleanup the descendant process tree"
                .to_string(),
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
            || !crate::harness::manual_st::vision_prompt_uses_labeled_attachment_fixture_passes()
            || !crate::agent::loop_impl::verification_turn_omits_consumed_images_fixture_passes()
            || !crate::agent::loop_impl::provider_chat_request_omits_consumed_images_fixture_passes(
            ))
    {
        diagnostics.push(
            "vision input items are not projected as Codex-style labeled image content, diagnostic source paths still leak into provider-visible workspace authority, or consumed images are reattached to verification / verification-repair ChatRequest messages".to_string(),
        );
    }

    if gate.gate_id == "preflight.workspace.absolute_turn_cwd_root_authority"
        && (!crate::workspace::discovery::workspace_discovery_absolute_root_authority_fixture_passes()
            || !crate::workspace::path_guard::path_guard_rejects_cross_workspace_absolute_remap_fixture_passes()
            || !crate::app::bootstrap::app_default_desktop_workspace_creation_fixture_passes())
    {
        diagnostics.push(
            "workspace discovery can still produce an empty or relative turn cwd/root authority, path guard can still remap a foreign absolute path into the active workspace by matching a directory name, or Desktop default workspace creation can still be treated as successful without filesystem setup evidence"
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
                "provider_metadata_mode_serializes_named_tool_choice",
                crate::agent::loop_impl::provider_metadata_mode_serializes_named_tool_choice_fixture_passes(),
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
                "loop_impl_docs_existing_target_grounding_fixture_domain_neutral",
                crate::agent::loop_impl::loop_impl_docs_existing_target_grounding_fixture_domain_neutral_fixture_passes(),
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
                "loop_impl_generated_test_source_reference_fixture_domain_neutral",
                crate::agent::loop_impl::loop_impl_generated_test_source_reference_fixture_domain_neutral_fixture_passes(),
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
                "loop_impl_singleton_write_argument_fixture_language_neutral",
                crate::agent::loop_impl::loop_impl_singleton_write_argument_fixture_language_neutral_fixture_passes(),
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
            || !crate::agent::repair_lane::repair_lane_source_target_identity_exact_fixture_passes()
            || !crate::agent::repair_lane::repair_lane_public_state_obligations_domain_neutral_fixture_passes()
            || !crate::agent::turn_decision::active_work_edit_authority_precedes_verification_rerun_fixture_passes()
            || !crate::agent::turn_decision::repair_lane_target_matches_active_work_authority_fixture_passes()
            || !crate::agent::turn_decision::repair_required_active_work_rejects_shell_only_surface_fixture_passes()
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
        if !crate::agent::repair_lane::repair_lane_source_target_identity_exact_fixture_passes() {
            failed_fixtures.push("repair_lane_source_target_identity_exact");
        }
        if !crate::agent::repair_lane::repair_lane_public_state_obligations_domain_neutral_fixture_passes() {
            failed_fixtures.push("repair_lane_public_state_obligations_domain_neutral");
        }
        if !crate::agent::turn_decision::active_work_edit_authority_precedes_verification_rerun_fixture_passes() {
            failed_fixtures.push("active_work_edit_authority_precedes_verification_rerun");
        }
        if !crate::agent::turn_decision::repair_lane_target_matches_active_work_authority_fixture_passes() {
            failed_fixtures.push("repair_lane_target_matches_active_work_authority");
        }
        if !crate::agent::turn_decision::repair_required_active_work_rejects_shell_only_surface_fixture_passes() {
            failed_fixtures.push("turn_decision_repair_required_edit_surface_required");
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
            || !crate::agent::loop_impl::answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes(
            )
            || !crate::agent::loop_impl::closeout_ready_final_response_timeout_guard_fixture_passes(
            )
            || !crate::agent::loop_impl::closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes(
            )
            || !crate::agent::loop_impl::invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes())
    {
        diagnostics.push(
            "clean closeout still requires a synthetic completion tool, no-executable-work answer-only turns can still reject final assistant messages, closeout can wait indefinitely for a provider final message after item-stream evidence is already satisfied, provider timeout can still synthesize final assistant text and complete the session, or an invalid-tool recovery shell success can still synthesize assistant text and complete without final assistant lifecycle authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.closeout.open_obligation_final_assistant_continuation_hook"
        && (!crate::harness::manual_st::final_assistant_open_obligation_not_clean_closeout_fixture_passes()
            || !crate::harness::manual_st::final_assistant_open_obligation_continuation_hook_fixture_passes()
            || !crate::harness::manual_st::open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes()
            || !crate::harness::manual_st::route_verification_waits_for_authored_artifacts_fixture_passes()
            || !crate::harness::manual_st::post_repair_route_verification_clears_stale_repair_fixture_passes()
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
            || !crate::harness::manual_st::terminalized_session_continuation_ledger_bounds_same_stage_recovery_fixture_passes()
            || !crate::harness::manual_st::route_terminal_cluster_uses_typed_tool_output_fixture_passes()
            || !crate::harness::manual_st::authoring_grounding_terminal_fail_stops_route_fixture_passes()
            || !crate::harness::manual_st::content_changing_authoring_no_progress_terminal_fail_stops_route_fixture_passes()
            || !crate::harness::manual_st::verification_repair_terminal_ledger_blocks_non_obligation_workspace_progress_fixture_passes()
            || !crate::harness::manual_st::unknown_terminal_reason_fail_stops_open_route_fixture_passes()
            || !crate::harness::manual_st::successful_closeout_continuation_rematerializes_case_verdict_fixture_passes()
            || !crate::harness::manual_st::route_terminal_verdict_rematerializes_from_case_results_fixture_passes()
            || !crate::harness::manual_st::completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes()
            || !crate::harness::manual_st::satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes()
            || !crate::agent::state::route_closeout_expected_artifacts_inventory_does_not_reopen_fixture_passes())
    {
        diagnostics.push(
            "runtime-completed, runtime-error, or runtime-terminal final assistant messages with open obligations are not converted into explicit text-only continuation user-turn items, expected artifact inventory can reopen non-stage authoring targets, current workspace artifacts fail to clear stale authoring obligations, satisfied docs repair can reopen route closeout, route verification can run before authored artifacts exist, post-repair route verification can leave stale repair authority active, closeout evidence can leak across stages, closeout verification does not use latest command evidence, verification pass evidence remains fresh after later content changes, runtime failures can keep stale missing-artifact closeout evidence, runtime open obligations bypass the closeout continuation budget, same-workspace no-progress continuations are not bounded, terminalized same-stage session continuations are not ledgered into bounded route failure, route terminal cluster classification can still depend on terminal prose instead of typed ToolOutput / RejectedToolProposal evidence, content-changing authoring no-progress terminal items can still re-dispatch same-stage continuation, unknown terminal reasons can still continue an open route, a successful closeout continuation does not re-materialize the case verdict from latest terminal evidence, or route-level verdict/stop_reason is not re-materialized from current case results".to_string(),
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
            || !crate::harness::manual_st::closeout_artifact_roles_use_language_adapter_fixture_passes()
            || !crate::harness::manual_st::closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes())
    {
        diagnostics.push(
            "failed verification closeout does not project a Codex-style verification-repair hook prompt, generated-test parse defects can still fall back to source repair targets, manual ST closeout artifact roles can still bypass the LanguageEvidenceAdapter, or closeout continuation budget is still scoped to the whole stage instead of the failure signature".to_string(),
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
            || !crate::harness::manual_st::route_authoring_content_paths_use_language_adapter_fixture_passes()
            || !crate::harness::manual_st::manual_st_route_omits_provider_defaults_without_explicit_override_fixture_passes()
            || !crate::harness::manual_st::stage_scoped_verification_commands_are_spec_owned_fixture_passes()
            || !crate::harness::manual_st::manual_st_visible_scenario_contract_prompt_fixture_passes())
    {
        diagnostics.push(
            "manual ST route evidence can still lose explicit session continuation, in-flight case progress/timeout boundary ownership, provider config inheritance, provider stream timeout classification, route-owned command timeout/wait policy, stage-scoped verification command ownership, fresh output-root ownership, bounded workspace manifest filtering, or prompt-visible scenario contract authority".to_string(),
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
            || !crate::agent::state::verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes()
            || !crate::agent::state::verification_repair_targets_from_state_uses_common_repair_authority_fixture_passes()
            || !crate::agent::language_evidence::code_like_test_source_projection_fixture_passes()
            || !crate::agent::state::generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes()
            || !crate::agent::state::generic_generated_test_line_column_call_site_targets_source_fixture_passes()
            || !crate::agent::state::verification_failure_active_work_outranks_stale_docs_route_fixture_passes()
            || !crate::agent::state::verification_failure_with_docs_reference_still_outranks_stale_docs_route_fixture_passes()
            || !crate::agent::state::public_command_contract_continuation_projects_compact_source_repair_fixture_passes()
            || !crate::agent::state::state_public_command_continuation_summary_uses_typed_observation_markers_fixture_passes()
            || !crate::agent::state::verification_repair_continuation_generated_test_parse_target_fixture_passes()
            || !crate::agent::state::state_residual_component_fixture_workflow_neutral_fixture_passes()
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
            || !crate::agent::state::state_generated_test_exception_overreach_fixture_domain_neutral_fixture_passes()
            || !crate::agent::state::generated_test_local_binding_contradiction_active_work_fixture_passes()
            || !crate::agent::state::post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes()
            || !crate::agent::loop_impl::operation_feedback_uses_active_work_targets_fixture_passes()
            || !crate::agent::repair_lane::source_owned_verification_repair_lane_fixture_passes()
            || !crate::agent::repair_lane::source_config_repair_lane_preserves_common_repair_authority_fixture_passes()
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
            || !crate::agent::repair_lane::repair_lane_source_target_identity_exact_fixture_passes()
            || !crate::agent::repair_lane::repair_lane_public_state_obligations_domain_neutral_fixture_passes()
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
                "verification_repair_continuation_existing_byproduct_path_is_not_repair_target",
                crate::agent::state::verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes,
            ),
            (
                "verification_repair_targets_from_state_uses_common_repair_authority",
                crate::agent::state::verification_repair_targets_from_state_uses_common_repair_authority_fixture_passes,
            ),
            (
                "code_like_test_source_projection",
                crate::agent::language_evidence::code_like_test_source_projection_fixture_passes,
            ),
            (
                "generic_generated_test_source_call_site_targets_source_without_python_suffix",
                crate::agent::state::generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes,
            ),
            (
                "generic_generated_test_line_column_call_site_targets_source",
                crate::agent::state::generic_generated_test_line_column_call_site_targets_source_fixture_passes,
            ),
            (
                "verification_failure_active_work_outranks_stale_docs_route",
                crate::agent::state::verification_failure_active_work_outranks_stale_docs_route_fixture_passes,
            ),
            (
                "verification_failure_with_docs_reference_still_outranks_stale_docs_route",
                crate::agent::state::verification_failure_with_docs_reference_still_outranks_stale_docs_route_fixture_passes,
            ),
            (
                "public_command_contract_continuation_projects_compact_source_repair",
                crate::agent::state::public_command_contract_continuation_projects_compact_source_repair_fixture_passes,
            ),
            (
                "state_public_command_continuation_summary_typed_observation",
                crate::agent::state::state_public_command_continuation_summary_uses_typed_observation_markers_fixture_passes,
            ),
            (
                "verification_repair_continuation_generated_test_parse_target",
                crate::agent::state::verification_repair_continuation_generated_test_parse_target_fixture_passes,
            ),
            (
                "state_residual_component_fixture_workflow_neutral",
                crate::agent::state::state_residual_component_fixture_workflow_neutral_fixture_passes,
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
                "state_generated_test_exception_overreach_fixture_domain_neutral",
                crate::agent::state::state_generated_test_exception_overreach_fixture_domain_neutral_fixture_passes,
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
                "source_owned_verification_repair_lane",
                crate::agent::repair_lane::source_owned_verification_repair_lane_fixture_passes,
            ),
            (
                "source_config_repair_lane_preserves_common_repair_authority",
                crate::agent::repair_lane::source_config_repair_lane_preserves_common_repair_authority_fixture_passes,
            ),
            (
                "source_owned_repair_lane_rejects_diagnostic_label_targets",
                crate::agent::repair_lane::source_owned_repair_lane_rejects_diagnostic_label_targets_fixture_passes,
            ),
            (
                "source_owned_repair_lane_stays_diagnostic",
                crate::agent::repair_lane::source_owned_repair_lane_stays_diagnostic_fixture_passes,
            ),
            (
                "source_owned_repair_lane_derives_source_from_generated_test_target",
                crate::agent::repair_lane::source_owned_repair_lane_derives_source_from_generated_test_target_fixture_passes,
            ),
            (
                "source_owned_repair_lane_canonicalizes_absolute_source_target",
                crate::agent::repair_lane::source_owned_repair_lane_canonicalizes_absolute_source_target_fixture_passes,
            ),
            (
                "public_output_stream_assertion_mismatch",
                crate::agent::repair_lane::public_output_stream_assertion_mismatch_fixture_passes,
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
                "repair_lane_source_target_identity_exact",
                crate::agent::repair_lane::repair_lane_source_target_identity_exact_fixture_passes,
            ),
            (
                "repair_lane_public_state_obligations_domain_neutral",
                crate::agent::repair_lane::repair_lane_public_state_obligations_domain_neutral_fixture_passes,
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
        if !crate::agent::state::state_docs_route_fixtures_are_workflow_neutral_fixture_passes() {
            diagnostics.push(
                "state docs route fixtures can still use component/Python/unittest surfaces instead of workflow-neutral source/test/docs roles".to_string(),
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
        if !crate::agent::state::state_docs_route_target_alias_identity_exact_fixture_passes() {
            diagnostics.push(
                "docs route target alias matching can still accept suffix-equivalent foreign or sibling-root paths instead of exact canonical workspace-relative identity".to_string(),
            );
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
        if !crate::agent::state::state_docs_closeout_continuation_exact_target_identity_fixture_passes()
        {
            diagnostics.push(
                "docs-only closeout continuation can still recognize deliverable targets through natural-language substring matching instead of exact normalized identity".to_string(),
            );
        }
        if !crate::agent::state::docs_route_verification_failure_preserves_docs_active_target_fixture_passes()
            || !crate::agent::repair_lane::docs_route_pending_verification_failure_projects_docs_repair_lane_fixture_passes()
        {
            diagnostics.push(
                "docs-only route verification failures can still replace the active docs deliverable with generic source/test repair authority".to_string(),
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
            || !crate::agent::loop_impl::docs_route_idempotent_write_no_progress_terminal_guard_fixture_passes()
            || !crate::agent::tool_orchestrator::docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes()
            || !crate::agent::loop_impl::docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_narrows_recovery_surface_fixture_passes()
            || !crate::agent::loop_impl::docs_route_budget_exhaustion_survives_partial_write_fixture_passes()
            || !crate::agent::loop_impl::loop_impl_docs_budget_edit_surface_fixture_language_neutral_fixture_passes()
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
            || !crate::agent::docs_semantic_contract::docs_semantic_contract_fixtures_are_workflow_neutral_fixture_passes()
            || !crate::agent::docs_semantic_contract::docs_semantic_claim_projection_operation_authority_fixture_passes()
            || !crate::agent::tool_orchestrator::docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes()
            || !crate::agent::prompt_assets::docs_route_reminder_projects_write_ready_boundary_fixture_passes())
    {
        diagnostics.push(
            "docs/spec authoring can still accept artifact progress that misses required latest-request claims or includes prohibited contradictory claims before handoff".to_string(),
        );
    }

    if gate.gate_id == "preflight.verification.public_command_contract_coverage" {
        let checks = [
            (
                "public_command_contract_fixture_passes",
                crate::agent::public_command_contract::public_command_contract_fixture_passes(),
            ),
            (
                "public_command_contract_apply_patch_uses_post_patch_content_fixture_passes",
                crate::agent::public_command_contract::public_command_contract_apply_patch_uses_post_patch_content_fixture_passes(),
            ),
            (
                "public_command_contract_helper_argv_operator_fixture_passes",
                crate::agent::public_command_contract::public_command_contract_helper_argv_operator_fixture_passes(),
            ),
            (
                "public_command_contract_feedback_projects_typed_missing_coverage_fixture_passes",
                crate::agent::public_command_contract::public_command_contract_feedback_projects_typed_missing_coverage_fixture_passes(),
            ),
            (
                "public_command_contract_fixtures_are_workflow_neutral_fixture_passes",
                crate::agent::public_command_contract::public_command_contract_fixtures_are_workflow_neutral_fixture_passes(),
            ),
            (
                "public_command_contract_failure_projects_compact_source_repair_fixture_passes",
                crate::agent::repair_lane::public_command_contract_failure_projects_compact_source_repair_fixture_passes(),
            ),
            (
                "public_command_contract_closeout_prompt_compacts_failure_evidence_fixture_passes",
                crate::harness::manual_st::public_command_contract_closeout_prompt_compacts_failure_evidence_fixture_passes(),
            ),
            (
                "public_command_contract_route_evidence_fixture_passes",
                crate::harness::manual_st::public_command_contract_route_evidence_fixture_passes(),
            ),
            (
                "route_verification_process_environment_fixture_passes",
                crate::harness::manual_st::route_verification_process_environment_fixture_passes(),
            ),
            (
                "loop_impl_verification_public_command_fixture_domain_neutral",
                crate::agent::loop_impl::loop_impl_verification_public_command_fixture_domain_neutral_fixture_passes(),
            ),
        ];
        let failed_fixtures = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed_fixtures.is_empty() {
            diagnostics.push(format!(
                "prompt/spec-visible public command contracts can still be satisfied by generated tests that omit argv/exit/stdout evidence, generated child subprocess tests can still omit bounded timeout authority, incremental patch validation can still ignore existing generated-test coverage, route evidence cannot represent expected nonzero command outcomes as passing contract checks, route-owned public command failures can still expand into raw traceback repair prompts instead of compact source repair, or route-owned verification can still diverge from shell tool UTF-8 process environment / output decoding authority; failed fixtures: {}",
                failed_fixtures.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.verification.command_correction_satisfies_obligation" {
        let checks = [
            (
                "corrected_verification_command_consumes_original_obligation",
                crate::agent::state::corrected_verification_command_consumes_original_obligation_fixture_passes(),
            ),
            (
                "singleton_verification_command_arguments_are_runtime_owned",
                crate::agent::loop_impl::singleton_verification_command_arguments_are_runtime_owned_fixture_passes(),
            ),
            (
                "verification_only_authority_narrows_to_exact_shell",
                crate::protocol::verification_only_authority_narrows_to_exact_shell_fixture_passes(),
            ),
            (
                "verification_requirements_use_generic_build_check",
                crate::agent::verification::verification_requirements_use_generic_build_check_fixture_passes(),
            ),
            (
                "verification_dotted_technology_token_not_file_target",
                crate::agent::verification::verification_dotted_technology_tokens_are_not_file_targets_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "corrected verification command executions can still pass without consuming the original required verification obligation, prompt/request action authority can still require the rejected literal command instead of the corrected executable form, or build/check requirements can still be represented as a Rust-specific upper lifecycle field; failed fixtures: {}",
                failed.join(", ")
            ));
        }
    }

    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && !crate::cli::render::cli_renderer_current_provider_profile_fixture_passes()
    {
        diagnostics.push(
            "CLI history renderer fixtures can still retain stale provider profile authority instead of the current closed-network LM Studio provider profile".to_string(),
        );
    }
    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && !crate::session::transcript::transcript_from_history_items_current_provider_profile_fixture_passes(
        )
    {
        diagnostics.push(
            "session transcript projection fixtures can still retain stale provider profile authority instead of the current closed-network LM Studio provider profile".to_string(),
        );
    }
    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && !crate::desktop::app::desktop_markdown_export_atomic_commit_fixture_passes()
    {
        diagnostics.push(
            "Desktop Markdown exports can still write GUI contract evidence artifacts directly instead of a same-directory atomic commit".to_string(),
        );
    }
    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && !crate::desktop::web_model::desktop_web_access_mode_typed_projection_fixture_passes()
    {
        diagnostics.push(
            "Desktop web access-mode projection can still depend on Rust Debug enum spelling instead of typed AccessMode authority".to_string(),
        );
    }
    if gate.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        && !desktop_transcript_row_kind_typed_projection_fixture_passes()
    {
        diagnostics.push(
            "Desktop transcript row preservation and folding can still depend on visible row kind strings instead of typed DesktopTranscriptRowKind authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.design.harness_engineering_roadmap_authority"
        && !harness_engineering_roadmap_current_authority_fixture_passes()
    {
        diagnostics.push(
            "Harness Engineering Roadmap can still project lowercase product authority, stale migration/case authority, direct model-gate/fresh-rerun next actions, or workflow-specific invariant roadmap wording instead of current moyAI typed lifecycle / active gate-family / route taxonomy authority".to_string(),
        );
    }

    if gate.gate_id == "preflight.tool_lifecycle.synthetic_feedback_not_verification_authority" {
        let checks = [
            (
                "synthetic_corrective_shell_feedback_is_not_verification_run",
                crate::agent::tool_orchestrator::synthetic_corrective_shell_feedback_is_not_verification_run_fixture_passes(),
            ),
            (
                "repair_supporting_context_is_scoped_to_typed_obligation",
                crate::agent::tool_orchestrator::repair_supporting_context_is_scoped_to_typed_obligation_fixture_passes(),
            ),
            (
                "synthetic_tool_feedback_preserves_real_verification_cluster",
                crate::agent::state::synthetic_tool_feedback_preserves_real_verification_cluster_fixture_passes(),
            ),
            (
                "truncated_tool_output_feedback_uses_registered_tool_surface",
                crate::tool::truncate::truncated_tool_output_feedback_uses_registered_tool_surface_fixture_passes(),
            ),
            (
                "no_progress_signature_projection_matches_schema",
                crate::harness::runtime_writer::no_progress_signature_projection_matches_schema_fixture_passes(),
            ),
            (
                "tool_no_progress_signature_schema_matches_runtime_projection",
                crate::harness::schema::tool_no_progress_signature_schema_matches_runtime_projection_fixture_passes(),
            ),
        ];
        let failed = checks
            .iter()
            .filter_map(|(name, passed)| (!*passed).then_some(*name))
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            diagnostics.push(format!(
                "synthetic corrective/tool-policy feedback can still overwrite real verification evidence, project unavailable registered tool feedback, or advertise no-progress signature schema/runtime markers without executable fixture coverage; failed fixtures: {}",
                failed.join(", ")
            ));
        }
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

fn desktop_transcript_row_kind_typed_projection_fixture_passes() -> bool {
    #[cfg(feature = "tauri-desktop")]
    {
        crate::desktop::query::desktop_transcript_row_kind_typed_projection_fixture_passes()
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
            && crate::desktop::artifact_projection::desktop_file_change_rows_preserve_call_id_fixture_passes()
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
                    model: PREFLIGHT_FIXTURE_MODEL.to_string(),
                    base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
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
                            model: PREFLIGHT_FIXTURE_MODEL.to_string(),
                            base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        }),
                    },
                    turn_id,
                    Some(0),
                    PREFLIGHT_FIXTURE_MODEL.to_string(),
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
                        model: PREFLIGHT_FIXTURE_MODEL.to_string(),
                        base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
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
                .compatibility_transcript(session.id)
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
            model: PREFLIGHT_FIXTURE_MODEL.to_string(),
            base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
            access_mode: AccessMode::AutoReview,
            sandbox: SandboxProfile::WorkspaceWrite,
            shell_family: ShellFamily::PowerShell,
            model_capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
                parallel_tool_calls: false,
                context_window: PREFLIGHT_FIXTURE_CONTEXT_WINDOW,
                max_output_tokens: PREFLIGHT_FIXTURE_MAX_OUTPUT_TOKENS,
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
         active_work=author tests/workflow.spec.ts. Runtime requires a call through one of the \
         currently allowed tools or a closeout-ready completion state before session completion.",
    )
}

pub fn structured_document_summary_generated_dependency_exclusion_fixture_passes() -> bool {
    crate::agent::state::structured_document_summary_skips_generated_dependency_targets_fixture_passes(
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
        r#"{"path":"src/workflow.rs","content":"previous workflow source content"}"#,
        "tests/workflow.spec.ts",
    ) {
        failed.push("stale_write_tool_call_replay_is_summary_only".to_string());
    }
    if !crate::agent::prompt::stale_write_tool_call_replay_omits_payload(
        r#"{"path":"src/workflow.rs","content":"pub fn workflow_advance() {}"}"#,
        "tests/workflow.spec.ts",
        "workflow_advance",
    ) {
        failed.push("stale_write_tool_call_replay_omits_payload".to_string());
    }
    if !crate::agent::prompt::stale_write_prelude_replay_omits_text(
        "tests/workflow.spec.ts",
        "src/workflow.rs",
    ) {
        failed.push("stale_write_prelude_replay_omits_text".to_string());
    }
    if !crate::agent::prompt::stale_todo_progress_replay_omits_prior_plan(
        "tests/workflow.spec.ts",
        "Create `src/workflow.rs` and `tests/workflow.spec.ts`.",
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
        "tests/workflow.spec.ts",
    ) {
        failed.push("exact_write_target_contract_projects_content_authority".to_string());
    }
    if !crate::agent::language_evidence::language_evidence_adapter_registry_fixture_passes() {
        failed.push("language_evidence_adapter_registry".to_string());
    }
    if !crate::agent::loop_impl::required_write_target_mismatch_feedback_projects_test_content_authority() {
        failed.push("required_write_target_mismatch_feedback_projects_test_content_authority".to_string());
    }
    if !crate::agent::loop_impl::concrete_write_required_action_narrows_broad_surface_fixture_passes(
    ) {
        failed.push("concrete_write_required_action_narrows_broad_surface".to_string());
    }
    if !crate::agent::loop_impl::exact_write_route_accepts_generated_test_content() {
        failed.push("exact_write_route_accepts_generated_test_content".to_string());
    }
    if !crate::agent::content_shape_contract::test_target_content_shape_projection_is_positive_and_forbidden() {
        failed.push("test_target_content_shape_projection_is_positive_and_forbidden".to_string());
    }
    if !crate::agent::content_shape_contract::test_target_subprocess_returncode_assertion_diagnostics_fixture_passes() {
        failed.push(
            "test_target_subprocess_returncode_assertion_diagnostics_contract".to_string(),
        );
    }
    if !crate::agent::content_shape_contract::test_target_module_qualified_reference_import_fixture_passes() {
        failed.push("test_target_module_qualified_reference_import_contract".to_string());
    }
    if !crate::agent::content_shape_contract::test_target_rejects_recursive_runner_self_invocation_fixture_passes() {
        failed.push("test_target_recursive_runner_self_invocation_rejected".to_string());
    }
    if !crate::agent::loop_impl::content_shape_mismatch_feedback_carries_positive_test_contract() {
        failed.push("content_shape_mismatch_feedback_carries_positive_test_contract".to_string());
    }
    if !crate::agent::tool_orchestrator::content_shape_mismatch_feedback_projects_current_action_fixture_passes() {
        failed.push("content_shape_mismatch_feedback_projects_current_action".to_string());
    }
    if !crate::agent::tool_result_classification::required_write_content_shape_mismatch_has_typed_progress_class() {
        failed.push("required_write_content_shape_mismatch_has_typed_progress_class".to_string());
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
    if !crate::agent::loop_impl::source_content_shape_rejects_duplicate_entrypoint_fixture_passes()
    {
        failed.push("source_content_shape_rejects_duplicate_entrypoint".to_string());
    }
    if !crate::agent::content_shape_contract::source_executable_shape_accepts_required_public_surface_fixture_passes() {
        failed.push("source_content_shape_accepts_required_public_surface".to_string());
    }
    if !crate::agent::content_shape_contract::generic_code_artifact_content_shape_rejects_serialized_payload_fixture_passes() {
        failed.push("generic_code_artifact_content_shape_rejects_serialized_payload".to_string());
    }
    if !crate::agent::tool_orchestrator::shell_file_change_content_shape_violation_is_no_progress_fixture_passes()
    {
        failed.push("shell_file_change_content_shape_violation_is_no_progress".to_string());
    }
    if !crate::agent::tool_orchestrator::file_change_non_utf8_after_state_is_content_shape_no_progress_fixture_passes()
    {
        failed.push("file_change_non_utf8_after_state_is_content_shape_no_progress".to_string());
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
    if !crate::agent::loop_impl::stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition_fixture_passes() {
        failed.push(
            "stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition"
                .to_string(),
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
    if !crate::agent::prompt::prompt_content_shape_projection_uses_adapter_contract_fixture_passes()
    {
        failed.push("prompt_content_shape_projection_uses_adapter_contract".to_string());
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
    if !crate::agent::prompt::prompt_provider_replay_residual_fixture_workflow_neutral_fixture_passes(
    ) {
        failed.push("prompt_provider_replay_residual_fixture_workflow_neutral".to_string());
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
        model: PREFLIGHT_FIXTURE_MODEL.to_string(),
        base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut stale_state = SessionStateSnapshot::default();
    stale_state
        .active_targets
        .push(camino::Utf8PathBuf::from("src/workflow.rs"));

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
                    text: "create tests/workflow.spec.ts".to_string(),
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
    provider_visible_text.contains("tests/workflow.spec.ts")
        && !provider_visible_text.contains("src/workflow.rs")
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
        model: PREFLIGHT_FIXTURE_MODEL.to_string(),
        base_url: PREFLIGHT_FIXTURE_BASE_URL.to_string(),
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
                output_text: "src/workflow.rs".to_string(),
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
                arguments: serde_json::json!({"path": "docs/unfinished-workflow.md"}),
                model_arguments: serde_json::json!({"path": "docs/unfinished-workflow.md"}),
                effective_arguments: serde_json::json!({"path": "docs/unfinished-workflow.md"}),
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
                if replayed == &call_id.to_string() && result.contains("src/workflow.rs")
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

fn failure_registry_markdown_json_sync_fixture_passes() -> bool {
    let manifest = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(repo_root) = manifest.parent() else {
        return false;
    };
    let registry_dir = repo_root.join("docs/testing");
    let markdown = match fs::read_to_string(registry_dir.join("FailureRegistry.md")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let json_text = match fs::read_to_string(registry_dir.join("failure-registry.json")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let json_value: serde_json::Value = match serde_json::from_str(&json_text) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(entries) = json_value
        .get("entries")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };

    let mut json_status_by_id = BTreeMap::new();
    let mut json_id_sequence = Vec::new();
    let mut json_source_reread_claim_ids = BTreeSet::new();
    let mut json_source_reread_ref_ids = BTreeSet::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !id.starts_with("FR22-") {
            continue;
        }
        let Some(status) = entry.get("status").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if json_status_by_id
            .insert(id.to_string(), status.to_string())
            .is_some()
        {
            return false;
        }
        if entry
            .get("direct_cause")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|cause| {
                cause.contains("No-grep")
                    || cause.contains("source reread")
                    || cause.contains("source-reread")
            })
        {
            json_source_reread_claim_ids.insert(id.to_string());
        }
        let expected_id_slug = id.to_ascii_lowercase();
        if entry
            .get("artifact_refs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|refs| {
                refs.iter().any(|value| {
                    value.as_str().is_some_and(|artifact_ref| {
                        let artifact_ref = artifact_ref.to_ascii_lowercase();
                        artifact_ref.contains(&expected_id_slug)
                            && artifact_ref.ends_with("source-reread-evidence.txt")
                    })
                })
            })
        {
            json_source_reread_ref_ids.insert(id.to_string());
        }
        json_id_sequence.push(id.to_string());
    }

    let mut markdown_status_by_id = BTreeMap::new();
    let mut markdown_id_sequence = Vec::new();
    let mut markdown_seen_ids = BTreeSet::new();
    let mut active_id: Option<String> = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed.strip_prefix("## ") {
            if id.starts_with("FR22-") {
                let id = id.to_string();
                if !markdown_seen_ids.insert(id.clone()) {
                    return false;
                }
                markdown_id_sequence.push(id.clone());
                active_id = Some(id);
            } else {
                active_id = None;
            }
            continue;
        }
        let Some(id) = active_id.as_ref() else {
            continue;
        };
        if let Some(rest) = trimmed.strip_prefix("- `status`: `") {
            let Some((status, _)) = rest.split_once('`') else {
                return false;
            };
            markdown_status_by_id.insert(id.clone(), status.to_string());
            active_id = None;
        }
    }

    let artifact_ids = match failure_registry_artifact_ids(repo_root) {
        Some(value) => value,
        None => return false,
    };

    !json_id_sequence.is_empty()
        && json_id_sequence == markdown_id_sequence
        && !markdown_status_by_id.is_empty()
        && markdown_status_by_id == json_status_by_id
        && json_source_reread_claim_ids.is_subset(&artifact_ids)
        && artifact_ids == json_source_reread_ref_ids
}

fn failure_registry_artifact_ids(repo_root: &Utf8Path) -> Option<BTreeSet<String>> {
    let sandbox = repo_root.join("project_sandbox");
    let entries = fs::read_dir(sandbox).ok()?;
    let mut ids = BTreeSet::new();
    for entry in entries {
        let entry = entry.ok()?;
        let file_type = entry.file_type().ok()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = fr22_id_from_artifact_dir_name(&name) else {
            continue;
        };
        let source_evidence = entry.path().join("source-reread-evidence.txt");
        if source_evidence.is_file() {
            ids.insert(id);
        }
    }
    Some(ids)
}

fn fr22_id_from_artifact_dir_name(name: &str) -> Option<String> {
    let mut parts = name.split('-');
    let prefix = parts.next()?;
    if prefix != "fr22" {
        return None;
    }
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    let sequence = parts.next()?;
    if year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || sequence.len() != 3
        || !year.chars().all(|ch| ch.is_ascii_digit())
        || !month.chars().all(|ch| ch.is_ascii_digit())
        || !day.chars().all(|ch| ch.is_ascii_digit())
        || !sequence.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some(format!("FR22-{year}-{month}-{day}-{sequence}"))
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
            gate_id: "preflight.harness.failure_registry_projection_sync".to_string(),
            purpose: "Failure Registry Markdown and JSON projections expose the same active FR22 ids and status values before root-fix continuation".to_string(),
            tier: 2,
            layer: PreflightLayer::HarnessReplay,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::FailureRegistryAuthority,
            fixture_id: "fixture.harness.failure_registry_projection_sync".to_string(),
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
            purpose: "successful content-changing repair FileChangeEvidence satisfies the current repair target, runner/dependency byproduct FileChangeEvidence stays evidence-only, clears evidence-only generated-test edit obligations for source-owned repair, promotes the next dispatch to exact verification rerun, and treats single-command verification as a runtime-owned required action instead of provider-compliance churn".to_string(),
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
            purpose: "failed verification outputs preserve prior obligation targets, use one repair-authority predicate for source/test/docs/source-config targets, reject byproduct paths as repair authority, and project typed source-owned repair authority from language-neutral source evidence as a single provider-visible edit target before the next provider dispatch".to_string(),
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
            purpose: "no-content write outputs, idempotent docs writes, and destructive no-op acknowledgement patches are typed no-progress / rejected results and cannot satisfy verification repair, docs closeout, or authoring progress".to_string(),
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
            purpose: "shell stdout/stderr display projection preserves text output through an explicit decode strategy, locale fallback, and language text I/O surface evidence without making one runner's environment variables the generic encoding authority".to_string(),
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
            gate_id: "preflight.tool_lifecycle.external_tool_surface_schema_validation"
                .to_string(),
            purpose: "external MCP tools/list descriptors are validated as typed tool-surface authority before model-visible metadata is constructed; malformed descriptors fail closed instead of being silently dropped".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.external_tool_surface_schema_validation"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.tool_lifecycle.full_access_configured_boundary_authority"
                .to_string(),
            purpose: "full_access suppresses confirmation only for requests admitted inside the configured workspace boundary, while outside-workspace, network, and protected-authority requests remain review-required".to_string(),
            tier: 2,
            layer: PreflightLayer::Flow,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::ToolLifecycleAuthority,
            fixture_id: "fixture.tool_lifecycle.full_access_configured_boundary_authority"
                .to_string(),
        },
        PreflightGate {
            gate_id: "preflight.vision.input_item_lifecycle_authority".to_string(),
            purpose: "vision attachments are provider-visible labeled image items for image-grounded authoring, then become typed evidence rather than reattached binary input for verification, verification-repair, and actual ChatRequest projection".to_string(),
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
            purpose: "verification repair authority is derived from VerificationFailureCluster evidence and sequence-primary canonical HistoryItem order rather than raw summary text or timestamp order".to_string(),
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
            gate_id: "preflight.design.flow_contract_harness_authority".to_string(),
            purpose: "flow/contract/harness responsibility map projects current manual ST path coordinates, current Codex/Roo Code/opencode comparison basis, and route-neutral current-authority language".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.flow_contract_harness_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.basic_design_authority".to_string(),
            purpose: "Basic Design projects current moyAI product/path authority, Codex-style typed lifecycle, single control-plane, event-sourced runtime, Desktop/App architecture, route-owned verification evidence, and current closed-network provider boundary".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.basic_design_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.feature_inventory_authority".to_string(),
            purpose: "Feature Inventory projects current moyAI capability taxonomy, typed action-family authority, route-owned verification evidence, Desktop/App and Agent Harness Engine capability surfaces, and current closed-network provider boundary".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.feature_inventory_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.desktop_app_basic_design_authority".to_string(),
            purpose: "Desktop App Basic Design projects current moyAI Desktop/App architecture through typed adapter ownership, canonical item projection, file-change and export evidence, current provider boundary, and historical implementation evidence boundary".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.desktop_app_basic_design_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.desktop_app_detailed_design_authority".to_string(),
            purpose: "Desktop App Detailed Design projects current moyAI Desktop/App typed adapter contracts, canonical item projection, file-change and Markdown export evidence, permission/provider/config projection boundaries, route-owned verification evidence, and historical implementation evidence boundaries".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.desktop_app_detailed_design_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.tui_design_authority".to_string(),
            purpose: "TUI Design projects current moyAI terminal adapter contracts, canonical item projection, terminal transcript projection, permission/provider/config projection boundaries, route-owned verification evidence, and historical implementation evidence boundary".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.tui_design_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.agent_harness_architecture_authority".to_string(),
            purpose: "Agent Harness Architecture projects state-machine and scenario-failure authority through typed action-family evidence, route-owned verification command obligations, VerificationRunResult, and language-neutral adapter evidence".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.agent_harness_architecture_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.agent_harness_components_authority".to_string(),
            purpose: "Agent Harness Components projects current moyAI Agent Harness Engine product authority across component-boundary design prose".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.agent_harness_components_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.agent_state_machine_authority".to_string(),
            purpose: "Agent State Machine projects current moyAI path authority, typed lifecycle transitions, action-family evidence, route-owned VerificationRunResult evidence, adapter-owned scenario evidence, and current Codex/Roo comparison basis".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.agent_state_machine_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.agent_harness_implementation_authority".to_string(),
            purpose: "Agent Harness Implementation Design projects current moyAI implementation authority through typed event-log replay, registries, deterministic harness contracts, and current implementation coverage".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.agent_harness_implementation_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.typed_contract_inventory_authority".to_string(),
            purpose: "typed contract inventory projects current moyAI path coordinates, provider-neutral OpenAI-compatible metadata/capability evidence, route-neutral scenario contract source paths, and typed verification command obligation evidence instead of exact language-specific command concern authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.typed_contract_inventory_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.thread_turn_item_protocol_authority".to_string(),
            purpose: "Thread/Turn/Item protocol projects current moyAI path coordinates, typed RequiredAction dispatch authority, implemented ToolLifecycleEnvelope / ToolOrchestrator ownership, active control-envelope gate naming, and current connection status while action strings and phase-era rollout wording remain renderer-only or non-authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.thread_turn_item_protocol_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.turn_decision_pipeline_authority".to_string(),
            purpose: "Turn Decision Pipeline projects current moyAI product authority for TurnControlEnvelope, ActionAuthority, ProjectionBundle, and typed continuation dispatch ownership".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.turn_decision_pipeline_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.root.spec_current_authority".to_string(),
            purpose: "README.md and ProjectBrief.md project current moyAI product/path authority for implementation, runtime contract, verification harness, and manual ST paths".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.root.spec_current_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.runtime_contracts_authority".to_string(),
            purpose: "Runtime Contracts projects current moyAI product authority and current moyAI/src owner module paths while keeping historical lowercase moyai mentions as incident evidence only".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.runtime_contracts_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.verification_harness_authority".to_string(),
            purpose: "Verification Harness projects current moyAI product authority and current moyAI/src plus moyAI/tests owner paths for harness design authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.verification_harness_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.harness_engineering_roadmap_authority".to_string(),
            purpose: "Harness Engineering Roadmap projects current moyAI Agent Harness Engine authority through typed lifecycle, active preflight gate-family, route taxonomy, workflow-neutral invariant roadmap, and user-overridden rerun boundary wording".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.harness_engineering_roadmap_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.item_lifecycle_detail_authority".to_string(),
            purpose: "Item Lifecycle Detail Design projects current moyAI product/path authority for the target item lifecycle and runtime/protocol/harness design surface".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.item_lifecycle_detail_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.tiered_quality_gates_authority".to_string(),
            purpose: "Tiered Quality Gates projects route taxonomy and invariant/artifact-role authority without behavior-stopper or case-primary quality-gate wording".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.tiered_quality_gates_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.current_authority_index_authority".to_string(),
            purpose: "Current Authority Index projects current moyAI product authority across invariant index prose without lowercase current-product moyai wording".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.current_authority_index_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.codex_lifecycle_conformance_audit_authority".to_string(),
            purpose: "Codex Lifecycle Conformance Audit projects current moyAI Codex-conformance authority through canonical item stream, ActionAuthority dispatch ownership, runtime capability hydration, projection separation, route-visible obligations, and historical incident evidence boundary".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.codex_lifecycle_conformance_audit_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.codex_control_plane_redesign_expanded_authority"
                .to_string(),
            purpose: "Codex Control-Plane Redesign Expanded Review projects current moyAI single control-plane authority through Thread / Turn / Item, TurnControlEnvelope, ActionAuthority, ProjectionBundle, ToolLifecycleEnvelope, route-owned obligations, app boundary projection, and active preflight families while historical incidents remain evidence only".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id:
                "fixture.design.codex_control_plane_redesign_expanded_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.codex_derived_redesign_recommendations_authority"
                .to_string(),
            purpose: "Codex-derived Redesign Recommendations project current moyAI adopted protocol-first runtime authority and historical recommendation boundary without future-phase rebuild sequencing, exact implementation examples, or provider/profile examples as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id:
                "fixture.design.codex_derived_redesign_recommendations_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.codex_ui_adoption_review_authority".to_string(),
            purpose: "Codex UI Adoption Review projects current moyAI Desktop/App typed adapter and canonical item projection authority without dated screenshot audit notes, implementation-history bullets, stale presentation-layer wording, or raw verification commands as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.codex_ui_adoption_review_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.codex_lifecycle_fr03_gap_analysis_authority".to_string(),
            purpose: "Codex Lifecycle FR03 Gap Analysis projects current moyAI rejected-proposal and candidate-repair lifecycle authority with a historical FR03 evidence boundary, without exact FR/case/tool/action/file/type examples or next-iteration sequencing as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.codex_lifecycle_fr03_gap_analysis_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.codex_itemlifecycle_authority".to_string(),
            purpose: "Codex Item Lifecycle Survey projects current moyAI Thread / Turn / Item lifecycle survey authority and historical incident evidence boundary without FR/case/tool/file/provider incident-ledger wording as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.codex_itemlifecycle_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.codex_reference_comparison_authority".to_string(),
            purpose: "Codex Reference Comparison projects current moyAI multi-reference lifecycle authority and historical incident evidence boundary without dated FR/case/tool/file/provider comparison wording as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.codex_reference_comparison_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.codex_structure_map_authority".to_string(),
            purpose: "Codex Structure Map projects current moyAI Codex structure authority and historical source evidence boundary without source-path ledger wording, exact type/tool primary keys, or stale current-state claims as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.codex_structure_map_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.contract_comparison_authority".to_string(),
            purpose: "Contract Comparison projects current moyAI Codex-first contract comparison authority and historical comparison evidence boundary without lowercase product authority, stale runtime owner paths, exact tool/action names, Python verification commands, provider/profile examples, or case artifact incident-ledger wording as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.contract_comparison_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.harness_comparison_authority".to_string(),
            purpose: "Harness Comparison projects current moyAI harness authority and historical harness evidence boundary without lowercase product authority, stale implementation paths, fixed route ordering, exact manual-ST artifacts, Python verification commands, provider/profile examples, or search-tool wording as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.harness_comparison_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.opencode_structure_map_authority".to_string(),
            purpose: "opencode Structure Map projects current moyAI opencode reference authority and historical source evidence boundary without lowercase product authority, phase-era sequencing, stale scope decisions, exact opencode source paths, exact tool/module names, or search-tool wording as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.opencode_structure_map_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.opencode_flow_description_authority".to_string(),
            purpose: "opencode Flow Description projects current moyAI opencode flow authority and historical flow evidence boundary without lowercase product authority, exact source paths, exact tool/module names, provider/prompt-family examples, dated incident comparison, case labels, or exact completion/todo/tool surfaces as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.opencode_flow_description_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.roocode_flow_description_authority".to_string(),
            purpose: "Roo Code Flow Description projects current moyAI Roo Code recovery-flow reference authority and historical flow evidence boundary without exact source paths, exact tool/class names, provider examples, dated incident comparison, lowercase product authority, or reserved adoption notes as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.roocode_flow_description_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.moyai_flow_description_authority".to_string(),
            purpose: "moyAI Flow Description projects current moyAI runtime flow authority and historical flow evidence boundary without lowercase product authority, exact source paths, legacy AgentLoop ownership, dated case evidence, exact commands, exact tool names, or incident-specific repair narratives as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.moyai_flow_description_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.opencode_contract_authority".to_string(),
            purpose: "opencode Contract projects current moyAI opencode contract reference authority and historical contract evidence boundary without lowercase product authority, exact opencode source paths, exact tool/module names, provider examples, dated incident comparison, or reading-order source lists as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.opencode_contract_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.roocode_contract_authority".to_string(),
            purpose: "Roo Code Contract projects current moyAI Roo Code recovery contract reference authority and historical contract evidence boundary without exact Roo source paths, exact tool/class names, provider examples, dated incident comparison, lowercase product authority, category adoption notes, or reading-order source lists as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.roocode_contract_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.opencode_verification_harness_authority".to_string(),
            purpose: "opencode Verification Harness projects current moyAI opencode deterministic harness reference authority and historical harness evidence boundary without exact opencode test source paths, exact test filenames, exact tool names, dated incident comparison, lowercase product authority, named scenario routes, or reading-order source lists as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.opencode_verification_harness_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.roocode_verification_harness_authority".to_string(),
            purpose: "Roo Code Verification Harness projects current moyAI Roo Code recovery harness reference authority and historical harness evidence boundary without exact Roo source paths, exact integration/unit test filenames, exact tool names, dated incident comparison, lowercase product authority, named scenario routes, local provider artifact wording, or reading-order source lists as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.roocode_verification_harness_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.survey.openclaw_runtime_authority".to_string(),
            purpose: "OpenClaw Runtime Survey projects current moyAI OpenClaw runtime reference authority and historical runtime evidence boundary without exact OpenClaw source paths, implementation names, provider/model examples, dated FR/case cluster mapping, adopted-difference work instructions, or reading-source lists as current authority".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.survey.openclaw_runtime_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.replay_first_harness_authority".to_string(),
            purpose: "Replay-first Harness projects current moyAI product/path authority and route-neutral replay invariant authority without case-primary fixture, restart, latest-stopper, exact unittest summary, or case-label evidence wording".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.replay_first_harness_authority".to_string(),
        },
        PreflightGate {
            gate_id: "preflight.design.run_store_event_log_authority".to_string(),
            purpose: "Run Store / Event Log projects current moyAI Agent Harness Engine authority, typed event feedback authority, and route-owned verification/E2E gate authority without prompt-string, representative-scenario, exact Python lane, incident-specific tool repetition, behavior-blocker, rerun-lane, or unittest-output wording".to_string(),
            tier: 2,
            layer: PreflightLayer::Contract,
            llm_mode: PreflightLlmMode::NoLlm,
            deterministic: true,
            status: PreflightGateStatus::Active,
            family: PreflightGateFamily::DesignAuthority,
            fixture_id: "fixture.design.run_store_event_log_authority".to_string(),
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
            authority_source: "runtime_event_stream explicit_turn_started turn_context_authority turn_item_stream terminal_turn_event canonical_history_item_stream HistoryItemAuthorityRole typed_tool_arguments protocol_tool_call_typed_arguments_authority legacy_display_arguments_not_canonical typed_file_change_evidence call_id_scoped_file_change_evidence typed_tool_output_success tool_output_blocked_action_projection protocol_pending_tool_lifecycle_blocked_action_absent app_resume_latest_user_sequence_primary_order protocol_mod_projection_fixture_current_provider_profile projection_items_not_provider_replay projection_items_not_state_reducer_authority".to_string(),
            required_refs: vec![
                "runtime_event_stream".to_string(),
                "explicit_turn_started".to_string(),
                "turn_context_authority".to_string(),
                "terminal_turn_event".to_string(),
                "canonical_history_item_stream".to_string(),
                "HistoryItemAuthorityRole".to_string(),
                "turn_item_stream".to_string(),
                "typed_tool_arguments".to_string(),
                "protocol_tool_call_typed_arguments_authority".to_string(),
                "legacy_display_arguments_not_canonical".to_string(),
                "typed_file_change_evidence".to_string(),
                "call_id_scoped_file_change_evidence".to_string(),
                "typed_tool_output_success".to_string(),
                "tool_output_blocked_action_projection".to_string(),
                "protocol_pending_tool_lifecycle_blocked_action_absent".to_string(),
                "app_resume_latest_user_sequence_primary_order".to_string(),
                "protocol_mod_projection_fixture_current_provider_profile".to_string(),
                "projection_items_not_provider_replay".to_string(),
                "projection_items_not_state_reducer_authority".to_string(),
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
            authority_source: "SqliteRuntimeUnitOfWork protocol_message_persistence session_status_transition protocol_runtime_projection todo_graph_replace_single_unit_of_work content_changing_tool_output_filechange_single_unit_of_work content_changing_tool_output_filechange_owner_coherence protocol_store_bundle_coherence protocol_store_latest_turn_position_unified_item_stream protocol_store_latest_turn_position_event_sourced_order protocol_store_single_item_append_order_atomic_commit runtime_event_publisher_observer_absence_best_effort run_event_projection_observer_absence_not_control_plane_failure harness_recorder_protocol_first_sink_composition native_harness_recorder_harness_only_protocol_sink_first preflight_protocol_fixture_current_provider_profile single_transaction terminal_event_last token_accounting_before_terminal emit_pre_recorded_after_commit".to_string(),
            required_refs: vec![
                "SqliteRuntimeUnitOfWork".to_string(),
                "protocol_message_persistence".to_string(),
                "session_status_transition".to_string(),
                "protocol_runtime_projection".to_string(),
                "todo_graph_replace_single_unit_of_work".to_string(),
                "content_changing_tool_output_filechange_single_unit_of_work".to_string(),
                "content_changing_tool_output_filechange_owner_coherence".to_string(),
                "protocol_store_bundle_coherence".to_string(),
                "protocol_store_latest_turn_position_unified_item_stream".to_string(),
                "protocol_store_latest_turn_position_event_sourced_order".to_string(),
                "protocol_store_single_item_append_order_atomic_commit".to_string(),
                "runtime_event_publisher_observer_absence_best_effort".to_string(),
                "run_event_projection_observer_absence_not_control_plane_failure".to_string(),
                "harness_recorder_protocol_first_sink_composition".to_string(),
                "native_harness_recorder_harness_only_protocol_sink_first".to_string(),
                "preflight_protocol_fixture_current_provider_profile".to_string(),
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
            fixture_id: "fixture.harness.failure_registry_projection_sync".to_string(),
            family: PreflightGateFamily::FailureRegistryAuthority,
            authority_source: "FailureRegistryMarkdown failure_registry_json project_sandbox_FR22_artifact_ids active_FR22_ids status_values same_ids same_status latest_entry_parity source_reread_artifact_ref_set_parity markdown_unique_section_ids json_markdown_section_sequence_parity json_markdown_status_sequence_parity dual_projection_authority preflight_gate_suite_docs_fixture_workflow_neutral failure_registry_header_current_entry_schema failure_registry_pending_status_verified_evidence_consistency failure_registry_implemented_pending_status_verified_evidence_consistency failure_registry_verified_status_pending_plan_consistency failure_registry_verified_status_future_action_plan_consistency failure_registry_verified_successor_rerun_lifecycle_text_absent failure_registry_rerun_exposed_status_verified_lifecycle failure_registry_verified_status_exposed_id_next_failure_consistency failure_registry_verified_rerun_pending_status_successor_evidence_consistency failure_registry_next_failure_exposed_status_successor_id_consistency failure_registry_verified_rerun_transient_status_absent failure_registry_pending_fresh_rerun_status_successor_evidence_consistency failure_registry_post_fix_verified_status_successor_projection_consistency failure_registry_verified_pending_status_blocker_resolution failure_registry_root_fix_in_progress_status_successor_evidence_consistency failure_registry_verified_harness_assessment_current_lifecycle failure_registry_regression_fixture_authority_workflow_neutral".to_string(),
            required_refs: vec![
                "FailureRegistryMarkdown".to_string(),
                "failure_registry_json".to_string(),
                "project_sandbox_FR22_artifact_ids".to_string(),
                "active_FR22_ids".to_string(),
                "status_values".to_string(),
                "same_ids".to_string(),
                "same_status".to_string(),
                "latest_entry_parity".to_string(),
                "source_reread_artifact_ref_set_parity".to_string(),
                "markdown_unique_section_ids".to_string(),
                "json_markdown_section_sequence_parity".to_string(),
                "json_markdown_status_sequence_parity".to_string(),
                "preflight_gate_suite_docs_fixture_workflow_neutral".to_string(),
                "failure_registry_header_current_entry_schema".to_string(),
                "failure_registry_pending_status_verified_evidence_consistency".to_string(),
                "failure_registry_implemented_pending_status_verified_evidence_consistency"
                    .to_string(),
                "failure_registry_verified_status_pending_plan_consistency".to_string(),
                "failure_registry_verified_status_future_action_plan_consistency".to_string(),
                "failure_registry_verified_successor_rerun_lifecycle_text_absent".to_string(),
                "failure_registry_rerun_exposed_status_verified_lifecycle".to_string(),
                "failure_registry_verified_status_exposed_id_next_failure_consistency"
                    .to_string(),
                "failure_registry_verified_rerun_pending_status_successor_evidence_consistency"
                    .to_string(),
                "failure_registry_next_failure_exposed_status_successor_id_consistency"
                    .to_string(),
                "failure_registry_verified_rerun_transient_status_absent".to_string(),
                "failure_registry_pending_fresh_rerun_status_successor_evidence_consistency"
                    .to_string(),
                "failure_registry_post_fix_verified_status_successor_projection_consistency"
                    .to_string(),
                "failure_registry_verified_pending_status_blocker_resolution".to_string(),
                "failure_registry_root_fix_in_progress_status_successor_evidence_consistency"
                    .to_string(),
                "failure_registry_verified_harness_assessment_current_lifecycle".to_string(),
                "failure_registry_regression_fixture_authority_workflow_neutral".to_string(),
            ],
            forbidden_refs: vec![
                "markdown_only_registry".to_string(),
                "json_only_registry".to_string(),
                "single_projection_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.flow_contract_harness_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "FlowContractHarnessResponsibilityMap current_manual_st_path moyAI/tests/manual_ST Codex_primary_comparison Roo_Code_local_llm_supplement opencode_third_reference representative_route_evidence_source route_neutral_current_design_authority".to_string(),
            required_refs: vec![
                "FlowContractHarnessResponsibilityMap".to_string(),
                "current_manual_st_path".to_string(),
                "moyAI/tests/manual_ST".to_string(),
                "Codex_primary_comparison".to_string(),
                "Roo_Code_local_llm_supplement".to_string(),
                "opencode_third_reference".to_string(),
                "representative_route_evidence_source".to_string(),
                "route_neutral_current_design_authority".to_string(),
            ],
            forbidden_refs: vec![
                "moyai/tests/manual_ST".to_string(),
                "OpenClaw".to_string(),
                "Case2_current_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.basic_design_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "BasicDesign moyAI moyAI/ Codex-style typed lifecycle single control-plane event-sourced runtime Desktop/App architecture Agent Harness Engine route-owned verification evidence current closed-network provider boundary historical phase evidence boundary".to_string(),
            required_refs: vec![
                "BasicDesign".to_string(),
                "moyAI".to_string(),
                "moyAI/".to_string(),
                "Codex-style typed lifecycle".to_string(),
                "single control-plane".to_string(),
                "event-sourced runtime".to_string(),
                "Desktop/App architecture".to_string(),
                "Agent Harness Engine".to_string(),
                "route-owned verification evidence".to_string(),
                "current closed-network provider boundary".to_string(),
                "historical phase evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/".to_string(),
                "moyai/src".to_string(),
                "Phase3 基本設計".to_string(),
                "Phase5 の入出力は CLI を先行実装".to_string(),
                "CLI 先行・単体バイナリ".to_string(),
                "local LM Studio".to_string(),
                "http://127.0.0.1:1234".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "list / glob / grep / read".to_string(),
                "`apply_patch` と whole-file `write`".to_string(),
                "exact crate selection は Phase4".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.feature_inventory_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "FeatureInventory moyAI capability taxonomy Codex-style typed lifecycle typed action-family authority adapter-owned evidence route-owned verification evidence Desktop/App architecture Agent Harness Engine current closed-network provider boundary historical reference evidence boundary non-adopted scope boundary".to_string(),
            required_refs: vec![
                "FeatureInventory".to_string(),
                "moyAI".to_string(),
                "capability taxonomy".to_string(),
                "Codex-style typed lifecycle".to_string(),
                "typed action-family authority".to_string(),
                "adapter-owned evidence".to_string(),
                "route-owned verification evidence".to_string(),
                "Desktop/App architecture".to_string(),
                "Agent Harness Engine".to_string(),
                "current closed-network provider boundary".to_string(),
                "historical reference evidence boundary".to_string(),
                "non-adopted scope boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/".to_string(),
                "Phase3".to_string(),
                "Phase5".to_string(),
                "CLI adapter を先行".to_string(),
                "list / glob / grep / read".to_string(),
                "`apply_patch`".to_string(),
                "whole-file `write`".to_string(),
                "LM Studio `/api/v1/models`".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "採否ラベル".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.desktop_app_basic_design_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "DesktopAppBasicDesign moyAI Desktop/App architecture typed adapter ownership canonical item projection file-change evidence Markdown export evidence current closed-network provider boundary historical implementation evidence boundary".to_string(),
            required_refs: vec![
                "DesktopAppBasicDesign".to_string(),
                "moyAI".to_string(),
                "Desktop/App architecture".to_string(),
                "typed adapter ownership".to_string(),
                "canonical item projection".to_string(),
                "file-change evidence".to_string(),
                "Markdown export evidence".to_string(),
                "current closed-network provider boundary".to_string(),
                "historical implementation evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai-desktop".to_string(),
                "moyai desktop".to_string(),
                "npm run build:desktop-web".to_string(),
                "cargo build --release --bin".to_string(),
                "127.0.0.1".to_string(),
                "旧 Slint".to_string(),
                "left navigation rail".to_string(),
                "right artifact / preview pane".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.desktop_app_detailed_design_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "DesktopAppDetailedDesign moyAI Desktop/App typed adapter contracts canonical item projection file-change evidence Markdown export evidence permission projection boundary provider projection boundary config projection boundary route-owned verification evidence current closed-network provider boundary historical implementation evidence boundary".to_string(),
            required_refs: vec![
                "DesktopAppDetailedDesign".to_string(),
                "moyAI".to_string(),
                "Desktop/App typed adapter contracts".to_string(),
                "canonical item projection".to_string(),
                "file-change evidence".to_string(),
                "Markdown export evidence".to_string(),
                "permission projection boundary".to_string(),
                "provider projection boundary".to_string(),
                "config projection boundary".to_string(),
                "route-owned verification evidence".to_string(),
                "current closed-network provider boundary".to_string(),
                "historical implementation evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/".to_string(),
                "moyai desktop".to_string(),
                "moyai-desktop".to_string(),
                "LM Studio".to_string(),
                "vLLM-MLX".to_string(),
                "`/api/v1/models`".to_string(),
                "`/health`".to_string(),
                "npm run".to_string(),
                "cargo build".to_string(),
                "cargo test".to_string(),
                "Implementation Order".to_string(),
                "left navigation rail".to_string(),
                "right artifact / preview pane".to_string(),
                "representative route".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.tui_design_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "TuiDesign moyAI terminal adapter contracts canonical item projection terminal transcript projection permission projection boundary provider projection boundary config projection boundary route-owned verification evidence current closed-network provider boundary historical implementation evidence boundary".to_string(),
            required_refs: vec![
                "TuiDesign".to_string(),
                "moyAI".to_string(),
                "terminal adapter contracts".to_string(),
                "canonical item projection".to_string(),
                "terminal transcript projection".to_string(),
                "permission projection boundary".to_string(),
                "provider projection boundary".to_string(),
                "config projection boundary".to_string(),
                "route-owned verification evidence".to_string(),
                "current closed-network provider boundary".to_string(),
                "historical implementation evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/".to_string(),
                "Phase6".to_string(),
                "Phase7".to_string(),
                "opencode".to_string(),
                "Roo Code".to_string(),
                "ratatui".to_string(),
                "crossterm".to_string(),
                "tui-textarea".to_string(),
                "read / list / grep / glob".to_string(),
                "http://192.168.10.103:1234".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "実装順序".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.agent_harness_architecture_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "AgentHarnessArchitecture typed content-changing action-family evidence support/context action-family evidence route-owned verification command obligation VerificationRunResult language/test-runner adapter evidence workflow-neutral scenario contract".to_string(),
            required_refs: vec![
                "AgentHarnessArchitecture".to_string(),
                "typed content-changing action-family evidence".to_string(),
                "support/context action-family evidence".to_string(),
                "route-owned verification command obligation".to_string(),
                "VerificationRunResult".to_string(),
                "language/test-runner adapter evidence".to_string(),
                "workflow-neutral scenario contract".to_string(),
            ],
            forbidden_refs: vec![
                "productive write / patch / read".to_string(),
                "exact command missing or stale evidence".to_string(),
                "concrete repair recorded and exact rerun due".to_string(),
                "python -m unittest failure".to_string(),
                "Action Routing / exact rerun".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.agent_harness_components_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source:
                "AgentHarnessComponents moyAI Agent Harness Engine component-boundary design authority"
                    .to_string(),
            required_refs: vec![
                "AgentHarnessComponents".to_string(),
                "moyAI".to_string(),
                "Agent Harness Engine".to_string(),
                "component-boundary design authority".to_string(),
            ],
            forbidden_refs: vec!["`moyai`".to_string(), "lowercase moyai".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.agent_state_machine_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "AgentStateMachine moyAI moyAI/src/session/state.rs typed action-family evidence route-owned verification command obligation VerificationRunResult language/test-runner adapter evidence Codex primary comparison Roo Code local-LLM supplement".to_string(),
            required_refs: vec![
                "AgentStateMachine".to_string(),
                "moyAI".to_string(),
                "moyAI/src/session/state.rs".to_string(),
                "typed action-family evidence".to_string(),
                "route-owned verification command obligation".to_string(),
                "VerificationRunResult".to_string(),
                "language/test-runner adapter evidence".to_string(),
                "Codex primary comparison".to_string(),
                "Roo Code local-LLM supplement".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "exact command".to_string(),
                "exact rerun".to_string(),
                "concrete repair".to_string(),
                "python -m unittest".to_string(),
                "OpenClaw".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.agent_harness_implementation_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "AgentHarnessImplementationDesign moyAI moyAI/src typed event-log replay Artifact Registry Contract Registry route-owned evidence deterministic harness contracts closed-network no-provider replay".to_string(),
            required_refs: vec![
                "AgentHarnessImplementationDesign".to_string(),
                "moyAI".to_string(),
                "moyAI/src".to_string(),
                "typed event-log replay".to_string(),
                "Artifact Registry".to_string(),
                "Contract Registry".to_string(),
                "route-owned evidence".to_string(),
                "deterministic harness contracts".to_string(),
                "closed-network no-provider replay".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/src".to_string(),
                "legacy adapter".to_string(),
                "pre-policy case2".to_string(),
                "provider / shell / workspace mutation".to_string(),
                "remaining open work".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.harness_engineering_roadmap_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "HarnessEngineeringRoadmap moyAI Agent Harness Engine typed lifecycle active preflight gate-family route taxonomy workflow-neutral invariant roadmap user-overridden model gate and fresh rerun boundary".to_string(),
            required_refs: vec![
                "HarnessEngineeringRoadmap".to_string(),
                "moyAI".to_string(),
                "Agent Harness Engine".to_string(),
                "typed lifecycle".to_string(),
                "active preflight gate-family".to_string(),
                "route taxonomy".to_string(),
                "workflow-neutral invariant roadmap".to_string(),
                "user-overridden model gate and fresh rerun boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Case2 contract migration".to_string(),
                "legacy adapter".to_string(),
                "collision 専用 guard".to_string(),
                "GameState.WON".to_string(),
                "pending admission".to_string(),
                "direct model gate next action".to_string(),
                "direct fresh rerun next action".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.typed_contract_inventory_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "TypedContractInventory moyAI current_manual_st_path moyAI/tests/manual_ST OpenAI-compatible metadata provider metadata mode capability evidence route_neutral_scenario_contract_source typed_verification_command_obligation_evidence".to_string(),
            required_refs: vec![
                "TypedContractInventory".to_string(),
                "moyAI".to_string(),
                "current_manual_st_path".to_string(),
                "moyAI/tests/manual_ST".to_string(),
                "OpenAI-compatible metadata".to_string(),
                "provider metadata mode".to_string(),
                "capability evidence".to_string(),
                "route_neutral_scenario_contract_source".to_string(),
                "typed_verification_command_obligation_evidence".to_string(),
            ],
            forbidden_refs: vec![
                "moyai/tests/manual_ST".to_string(),
                "LM Studio metadata summary".to_string(),
                "case*/spec.md".to_string(),
                "exact `py_compile` / `python -m unittest`".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.codex_control_plane_redesign_expanded_authority"
                .to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexControlPlaneRedesignExpanded current moyAI Thread / Turn / Item protocol TurnControlEnvelope ActionAuthority ProjectionBundle ToolLifecycleEnvelope runtime capability hydration event-sourced runtime route-owned obligations app boundary projection active preflight gate families historical evidence boundary Codex primary comparison Roo Code local-LLM supplement opencode third reference".to_string(),
            required_refs: vec![
                "CodexControlPlaneRedesignExpanded".to_string(),
                "current moyAI".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "TurnControlEnvelope".to_string(),
                "ActionAuthority".to_string(),
                "ProjectionBundle".to_string(),
                "ToolLifecycleEnvelope".to_string(),
                "runtime capability hydration".to_string(),
                "event-sourced runtime".to_string(),
                "route-owned obligations".to_string(),
                "app boundary projection".to_string(),
                "active preflight gate families".to_string(),
                "historical evidence boundary".to_string(),
                "Codex primary comparison".to_string(),
                "Roo Code local-LLM supplement".to_string(),
                "opencode third reference".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "current `moyai`".to_string(),
                "FR2 cluster".to_string(),
                "case2b".to_string(),
                "OpenClaw".to_string(),
                "current tool set: read / write / apply_patch / shell".to_string(),
                "tool_choice=required".to_string(),
                "implementation slice".to_string(),
                "2026-05-04".to_string(),
                "2026-05-05".to_string(),
                "old `AgentLoop`".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.codex_derived_redesign_recommendations_authority"
                .to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexDerivedRedesignRecommendations current `moyAI` historical recommendation boundary adopted protocol-first runtime Thread / Turn / Item protocol single control-plane ToolOrchestrator-owned lifecycle protocol store app boundary projection active preflight gate families route-owned verification evidence".to_string(),
            required_refs: vec![
                "CodexDerivedRedesignRecommendations".to_string(),
                "current `moyAI`".to_string(),
                "historical recommendation boundary".to_string(),
                "adopted protocol-first runtime".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "single control-plane".to_string(),
                "ToolOrchestrator-owned lifecycle".to_string(),
                "protocol store".to_string(),
                "app boundary projection".to_string(),
                "active preflight gate families".to_string(),
                "route-owned verification evidence".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Phase12 live rerun restart point".to_string(),
                "Phase R1".to_string(),
                "Phase R2".to_string(),
                "Phase R3".to_string(),
                "Phase R4".to_string(),
                "Phase R5".to_string(),
                "Phase R6".to_string(),
                "ThreadOp".to_string(),
                "TurnEngine".to_string(),
                "rollout/*.jsonl".to_string(),
                "thread/start".to_string(),
                "tool/approval/respond".to_string(),
                "current tool set".to_string(),
                "LM Studio metadata".to_string(),
                "OpenClaw".to_string(),
                "Roo Code 型".to_string(),
                "Rebuild vs incremental".to_string(),
                "次の大きな設計作業".to_string(),
                "作り直すべき".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.codex_ui_adoption_review_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexUiAdoptionReview current `moyAI` Desktop/App typed adapter boundary canonical item projection artifact pane diff visibility composer dispatch context left navigation shell local command palette keyboard shortcut overlay non-adopted cloud/account/plugin/realtime boundary historical screenshot evidence boundary".to_string(),
            required_refs: vec![
                "CodexUiAdoptionReview".to_string(),
                "current `moyAI`".to_string(),
                "Desktop/App typed adapter boundary".to_string(),
                "canonical item projection".to_string(),
                "artifact pane".to_string(),
                "diff visibility".to_string(),
                "composer dispatch context".to_string(),
                "left navigation shell".to_string(),
                "local command palette".to_string(),
                "keyboard shortcut overlay".to_string(),
                "non-adopted cloud/account/plugin/realtime boundary".to_string(),
                "historical screenshot evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "Date: 2026-05-12".to_string(),
                "C:/Users/".to_string(),
                "moyai-feature".to_string(),
                "Workspace grep exists".to_string(),
                "Slint".to_string(),
                "FR10".to_string(),
                "Implemented on".to_string(),
                "Follow-up screenshot comparison".to_string(),
                "GUI screenshots".to_string(),
                "project_sandbox/manual-st-case1".to_string(),
                "cargo fmt --all --check".to_string(),
                "cargo check".to_string(),
                "cargo test --lib".to_string(),
                "cargo build --bin moyai-desktop".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.codex_lifecycle_fr03_gap_analysis_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexLifecycleFr03GapAnalysis current `moyAI` historical FR03 evidence boundary Thread / Turn / Item protocol rejected proposal lifecycle candidate repair evidence ToolOrchestrator-owned lifecycle call-id-symmetric tool output typed candidate admission compaction continuity active preflight gate families".to_string(),
            required_refs: vec![
                "CodexLifecycleFr03GapAnalysis".to_string(),
                "current `moyAI`".to_string(),
                "historical FR03 evidence boundary".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "rejected proposal lifecycle".to_string(),
                "candidate repair evidence".to_string(),
                "ToolOrchestrator-owned lifecycle".to_string(),
                "call-id-symmetric tool output".to_string(),
                "typed candidate admission".to_string(),
                "compaction continuity".to_string(),
                "active preflight gate families".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "FR03-2026-05-04-001".to_string(),
                "case1".to_string(),
                "calculator.py".to_string(),
                "`ValueError`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "codex/codex-rs/".to_string(),
                "moyai/src/".to_string(),
                "OpenClaw-style".to_string(),
                "Roo Code-style".to_string(),
                "The next iteration-process steps".to_string(),
                "Required Core Route A".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.codex_itemlifecycle_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexItemLifecycleSurvey current `moyAI` historical incident evidence boundary Thread / Turn / Item protocol canonical item stream submitted model action lifecycle call-id-scoped tool output rejected proposal evidence failed edit recovery item final assistant message lifecycle compaction continuity route/harness boundary provider-boundary classification active preflight gate families".to_string(),
            required_refs: vec![
                "CodexItemLifecycleSurvey".to_string(),
                "current `moyAI`".to_string(),
                "historical incident evidence boundary".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "canonical item stream".to_string(),
                "submitted model action lifecycle".to_string(),
                "call-id-scoped tool output".to_string(),
                "rejected proposal evidence".to_string(),
                "failed edit recovery item".to_string(),
                "final assistant message lifecycle".to_string(),
                "compaction continuity".to_string(),
                "route/harness boundary".to_string(),
                "provider-boundary classification".to_string(),
                "active preflight gate families".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "`rg`".to_string(),
                "grep".to_string(),
                "FR20-".to_string(),
                "FR21-".to_string(),
                "FR22-".to_string(),
                "case1".to_string(),
                "case2a".to_string(),
                "case2c".to_string(),
                "case3".to_string(),
                "calculator.py".to_string(),
                "test_calculator.py".to_string(),
                "space_invader.py".to_string(),
                "LM Studio".to_string(),
                "vLLM-MLX".to_string(),
                "`apply_patch`".to_string(),
                "`todowrite`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "python -m unittest".to_string(),
                "PowerShell".to_string(),
                "Get-Content".to_string(),
                "2026-05-".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.codex_reference_comparison_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexReferenceComparison current `moyAI` historical incident evidence boundary Thread / Turn / Item protocol single control-plane typed tool lifecycle event-sourced runtime compaction continuity local-LLM recovery boundary harness engineering Codex primary reference Roo Code recovery reference opencode scope reference OpenClaw execution reference".to_string(),
            required_refs: vec![
                "CodexReferenceComparison".to_string(),
                "current `moyAI`".to_string(),
                "historical incident evidence boundary".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "single control-plane".to_string(),
                "typed tool lifecycle".to_string(),
                "event-sourced runtime".to_string(),
                "compaction continuity".to_string(),
                "local-LLM recovery boundary".to_string(),
                "harness engineering".to_string(),
                "Codex primary reference".to_string(),
                "Roo Code recovery reference".to_string(),
                "opencode scope reference".to_string(),
                "OpenClaw execution reference".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "2026-05-25".to_string(),
                "vLLM-MLX".to_string(),
                "LM Studio".to_string(),
                "FR10".to_string(),
                "FR20-".to_string(),
                "FR21-".to_string(),
                "FR22-".to_string(),
                "case1".to_string(),
                "calculator.py".to_string(),
                "test_calculator.py".to_string(),
                "loop_impl.rs".to_string(),
                "`apply_patch`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "unittest".to_string(),
                "次に更新すべき設計文書".to_string(),
                "次の修正サイクル".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.codex_structure_map_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexStructureMap current `moyAI` historical source evidence boundary protocol boundary turn context canonical item stream tool lifecycle ownership permission and sandbox model compaction continuity memory boundary persistence split app boundary harness engineering non-adopted breadth boundary".to_string(),
            required_refs: vec![
                "CodexStructureMap".to_string(),
                "current `moyAI`".to_string(),
                "historical source evidence boundary".to_string(),
                "protocol boundary".to_string(),
                "turn context".to_string(),
                "canonical item stream".to_string(),
                "tool lifecycle ownership".to_string(),
                "permission and sandbox model".to_string(),
                "compaction continuity".to_string(),
                "memory boundary".to_string(),
                "persistence split".to_string(),
                "app boundary".to_string(),
                "harness engineering".to_string(),
                "non-adopted breadth boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "codex/codex-rs/".to_string(),
                "`Op`".to_string(),
                "`EventMsg`".to_string(),
                "`UserTurn`".to_string(),
                "`ResponseItem`".to_string(),
                "`TurnItem`".to_string(),
                "`apply_patch`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "gpt-5".to_string(),
                "/responses/compact".to_string(),
                "lmstudio".to_string(),
                "ollama".to_string(),
                "まだ protocol-first ではない".to_string(),
                "agent loop 本体".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.contract_comparison_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "ContractComparison current `moyAI` historical comparison evidence boundary Codex primary contract reference Thread / Turn / Item protocol single control-plane canonical item stream typed tool lifecycle Roo Code recovery reference opencode session hardening reference current `moyAI` contract authority closed-network provider boundary route-owned verification evidence harness engineering".to_string(),
            required_refs: vec![
                "ContractComparison".to_string(),
                "current `moyAI`".to_string(),
                "historical comparison evidence boundary".to_string(),
                "Codex primary contract reference".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "single control-plane".to_string(),
                "canonical item stream".to_string(),
                "typed tool lifecycle".to_string(),
                "Roo Code recovery reference".to_string(),
                "opencode session hardening reference".to_string(),
                "current `moyAI` contract authority".to_string(),
                "closed-network provider boundary".to_string(),
                "route-owned verification evidence".to_string(),
                "harness engineering".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/src/".to_string(),
                "AgentLoop".to_string(),
                "loop_impl.rs".to_string(),
                "SessionPrompt.runLoop".to_string(),
                "presentAssistantMessage".to_string(),
                "validateToolUse".to_string(),
                "ToolRepetitionDetector".to_string(),
                "attempt_completion".to_string(),
                "case1".to_string(),
                "case2".to_string(),
                "case3".to_string(),
                "project_sandbox/manual-st".to_string(),
                "calculator.py".to_string(),
                "test_calculator.py".to_string(),
                "space_invader.py".to_string(),
                "python -m unittest".to_string(),
                "unittest".to_string(),
                "LM Studio".to_string(),
                "vLLM-MLX".to_string(),
                "`todowrite`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "DOOM_LOOP_THRESHOLD".to_string(),
                "2026-04-".to_string(),
                "2026-05-".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.harness_comparison_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "HarnessComparison current `moyAI` historical harness evidence boundary Codex primary harness reference Thread / Turn / Item protocol canonical item stream active preflight gate families deterministic replay route-owned evidence Roo Code recovery harness reference opencode session harness reference current `moyAI` harness authority closed-network provider boundary manual ST evidence boundary harness engineering".to_string(),
            required_refs: vec![
                "HarnessComparison".to_string(),
                "current `moyAI`".to_string(),
                "historical harness evidence boundary".to_string(),
                "Codex primary harness reference".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "canonical item stream".to_string(),
                "active preflight gate families".to_string(),
                "deterministic replay".to_string(),
                "route-owned evidence".to_string(),
                "Roo Code recovery harness reference".to_string(),
                "opencode session harness reference".to_string(),
                "current `moyAI` harness authority".to_string(),
                "closed-network provider boundary".to_string(),
                "manual ST evidence boundary".to_string(),
                "harness engineering".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/src/".to_string(),
                "moyai/tests/manual_ST/case".to_string(),
                "case1".to_string(),
                "case2".to_string(),
                "case3".to_string(),
                "case4".to_string(),
                "case5".to_string(),
                "case6".to_string(),
                "case7".to_string(),
                "project_sandbox/manual-st".to_string(),
                "python -m unittest".to_string(),
                "unittest".to_string(),
                "py_compile".to_string(),
                "LM Studio".to_string(),
                "vLLM-MLX".to_string(),
                "bun:test".to_string(),
                "vitest".to_string(),
                "stream-json".to_string(),
                "attempt_completion".to_string(),
                "update_todo_list".to_string(),
                "validateToolUse".to_string(),
                "ToolRepetitionDetector".to_string(),
                "`todowrite`".to_string(),
                "`write`".to_string(),
                "`shell`".to_string(),
                "`read`".to_string(),
                "tool_choice=required".to_string(),
                "write.path".to_string(),
                "image_count".to_string(),
                "image_bytes".to_string(),
                "space_invader.py".to_string(),
                "calculator.py".to_string(),
                "test_calculator.py".to_string(),
                "README.md".to_string(),
                "grep-based".to_string(),
                "2026-04-".to_string(),
                "2026-05-".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.opencode_structure_map_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "OpencodeStructureMap current `moyAI` historical source evidence boundary opencode session engine reference opencode tool registry reference opencode workspace policy reference opencode storage and event reference opencode provider capability reference current `moyAI` opencode reference authority Codex-style lifecycle alignment closed-network adoption boundary non-adopted ecosystem boundary".to_string(),
            required_refs: vec![
                "OpencodeStructureMap".to_string(),
                "current `moyAI`".to_string(),
                "historical source evidence boundary".to_string(),
                "opencode session engine reference".to_string(),
                "opencode tool registry reference".to_string(),
                "opencode workspace policy reference".to_string(),
                "opencode storage and event reference".to_string(),
                "opencode provider capability reference".to_string(),
                "current `moyAI` opencode reference authority".to_string(),
                "Codex-style lifecycle alignment".to_string(),
                "closed-network adoption boundary".to_string(),
                "non-adopted ecosystem boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Phase2".to_string(),
                "Phase3".to_string(),
                "Phase5".to_string(),
                "Phase6".to_string(),
                "Phase7".to_string(),
                "opencode/packages/opencode/src/".to_string(),
                "opencode/packages/opencode/bin/opencode".to_string(),
                "src/tool/grep.ts".to_string(),
                "src/tool/bash.ts".to_string(),
                "src/tool/read.ts".to_string(),
                "src/tool/apply_patch.ts".to_string(),
                "src/session/prompt.ts".to_string(),
                "src/session/processor.ts".to_string(),
                "src/provider/provider.ts".to_string(),
                "src/provider/models.ts".to_string(),
                "src/file/index.ts".to_string(),
                "grep".to_string(),
                "Hono".to_string(),
                "yargs".to_string(),
                "Node".to_string(),
                "plugin system".to_string(),
                "MCP".to_string(),
                "現行仕様では採用しない".to_string(),
                "現行仕様では対象外".to_string(),
                "CLI 中心".to_string(),
                "将来候補".to_string(),
                "保留".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.opencode_flow_description_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "OpencodeFlowDescription current `moyAI` historical flow evidence boundary opencode session loop reference user input normalization tool surface resolution stream processor itemization retry and compaction boundary subtask session boundary todo progress projection boundary completion boundary Codex-style flow alignment current `moyAI` flow authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "OpencodeFlowDescription".to_string(),
                "current `moyAI`".to_string(),
                "historical flow evidence boundary".to_string(),
                "opencode session loop reference".to_string(),
                "user input normalization".to_string(),
                "tool surface resolution".to_string(),
                "stream processor itemization".to_string(),
                "retry and compaction boundary".to_string(),
                "subtask session boundary".to_string(),
                "todo progress projection boundary".to_string(),
                "completion boundary".to_string(),
                "Codex-style flow alignment".to_string(),
                "current `moyAI` flow authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "opencode/packages/opencode/".to_string(),
                "src/session/".to_string(),
                "src/tool/".to_string(),
                "src/cli/".to_string(),
                "`todowrite`".to_string(),
                "`apply_patch`".to_string(),
                "`edit`".to_string(),
                "`write`".to_string(),
                "`codesearch`".to_string(),
                "`websearch`".to_string(),
                "DOOM_LOOP_THRESHOLD".to_string(),
                "case5".to_string(),
                "case".to_string(),
                "2026-04-".to_string(),
                "LM Studio".to_string(),
                "Roo Code".to_string(),
                "GPT".to_string(),
                "Claude".to_string(),
                "Google".to_string(),
                "beast".to_string(),
                "anthropic".to_string(),
                "OPENCODE".to_string(),
                "MCP".to_string(),
                "plugin".to_string(),
                "Read tool".to_string(),
                "provider family".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.roocode_flow_description_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "RooCodeFlowDescription current `moyAI` historical flow evidence boundary Roo Code recovery-flow reference task state loop reference model-visible recovery state todo reinjection boundary completion gate boundary repetition guard boundary approval category boundary feedback loop boundary Codex-style control-plane alignment current `moyAI` recovery-flow authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "RooCodeFlowDescription".to_string(),
                "current `moyAI`".to_string(),
                "historical flow evidence boundary".to_string(),
                "Roo Code recovery-flow reference".to_string(),
                "task state loop reference".to_string(),
                "model-visible recovery state".to_string(),
                "todo reinjection boundary".to_string(),
                "completion gate boundary".to_string(),
                "repetition guard boundary".to_string(),
                "approval category boundary".to_string(),
                "feedback loop boundary".to_string(),
                "Codex-style control-plane alignment".to_string(),
                "current `moyAI` recovery-flow authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Roo-Code/".to_string(),
                "src/".to_string(),
                "`update_todo_list`".to_string(),
                "`attempt_completion`".to_string(),
                "ToolRepetitionDetector".to_string(),
                "AttemptCompletionTool".to_string(),
                "UpdateTodoListTool".to_string(),
                "ClineProvider".to_string(),
                "Task.ts".to_string(),
                "case5".to_string(),
                "2026-04-".to_string(),
                "LM Studio".to_string(),
                "MCP".to_string(),
                "reserved".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.moyai_flow_description_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "MoyaiFlowDescription current `moyAI` historical flow evidence boundary current runtime flow authority Thread / Turn / Item protocol single control-plane canonical item stream state reducer authority ToolOrchestrator lifecycle prompt projection boundary verification lane boundary completion and handoff boundary compaction continuity boundary active preflight gate families closed-network provider boundary".to_string(),
            required_refs: vec![
                "MoyaiFlowDescription".to_string(),
                "current `moyAI`".to_string(),
                "historical flow evidence boundary".to_string(),
                "current runtime flow authority".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "single control-plane".to_string(),
                "canonical item stream".to_string(),
                "state reducer authority".to_string(),
                "ToolOrchestrator lifecycle".to_string(),
                "prompt projection boundary".to_string(),
                "verification lane boundary".to_string(),
                "completion and handoff boundary".to_string(),
                "compaction continuity boundary".to_string(),
                "active preflight gate families".to_string(),
                "closed-network provider boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "moyai/src/".to_string(),
                "src/".to_string(),
                "AgentLoop".to_string(),
                "loop_impl.rs".to_string(),
                "`todowrite`".to_string(),
                "`shell`".to_string(),
                "`write`".to_string(),
                "case1".to_string(),
                "case2".to_string(),
                "case5".to_string(),
                "case7".to_string(),
                "2026-04-".to_string(),
                "project_sandbox/manual-st".to_string(),
                "space_invader.py".to_string(),
                "python -m".to_string(),
                "LM Studio".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.opencode_contract_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "OpencodeContract current `moyAI` historical contract evidence boundary opencode session contract reference session loop contract stream item persistence retry visibility contract compaction replay boundary permission rules boundary todo persistence boundary loop safety boundary Codex-style contract alignment current `moyAI` opencode contract reference authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "OpencodeContract".to_string(),
                "current `moyAI`".to_string(),
                "historical contract evidence boundary".to_string(),
                "opencode session contract reference".to_string(),
                "session loop contract".to_string(),
                "stream item persistence".to_string(),
                "retry visibility contract".to_string(),
                "compaction replay boundary".to_string(),
                "permission rules boundary".to_string(),
                "todo persistence boundary".to_string(),
                "loop safety boundary".to_string(),
                "Codex-style contract alignment".to_string(),
                "current `moyAI` opencode contract reference authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "opencode/packages/opencode/".to_string(),
                "src/session/".to_string(),
                "src/tool/".to_string(),
                "src/agent/".to_string(),
                "`todowrite`".to_string(),
                "SessionPrompt".to_string(),
                "SessionProcessor".to_string(),
                "SessionRetry".to_string(),
                "SessionCompaction".to_string(),
                "case5".to_string(),
                "2026-04-".to_string(),
                "LM Studio".to_string(),
                "prompt family".to_string(),
                "読む順番".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.roocode_contract_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "RooCodeContract current `moyAI` historical contract evidence boundary Roo Code recovery contract reference task state ownership boundary per-turn environment reinjection todo persistence contract completion gate contract tool validation boundary repetition guard boundary multimodal input boundary approval category reference Codex-style contract alignment current `moyAI` Roo Code recovery contract authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "RooCodeContract".to_string(),
                "current `moyAI`".to_string(),
                "historical contract evidence boundary".to_string(),
                "Roo Code recovery contract reference".to_string(),
                "task state ownership boundary".to_string(),
                "per-turn environment reinjection".to_string(),
                "todo persistence contract".to_string(),
                "completion gate contract".to_string(),
                "tool validation boundary".to_string(),
                "repetition guard boundary".to_string(),
                "multimodal input boundary".to_string(),
                "approval category reference".to_string(),
                "Codex-style contract alignment".to_string(),
                "current `moyAI` Roo Code recovery contract authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Roo-Code/".to_string(),
                "src/core/".to_string(),
                "`update_todo_list`".to_string(),
                "`attempt_completion`".to_string(),
                "Task.ts".to_string(),
                "UpdateTodoListTool".to_string(),
                "AttemptCompletionTool".to_string(),
                "ToolRepetitionDetector".to_string(),
                "validateToolUse".to_string(),
                "LM Studio".to_string(),
                "2026-04-".to_string(),
                "読む順番".to_string(),
                "read / write / execute".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.opencode_verification_harness_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "OpencodeVerificationHarness current `moyAI` historical harness evidence boundary opencode deterministic harness reference isolated project fixture boundary fake provider control boundary component session harness boundary tool and permission harness boundary server route harness boundary subsystem split boundary deterministic runtime evidence Codex-style harness alignment current `moyAI` opencode deterministic harness authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "OpencodeVerificationHarness".to_string(),
                "current `moyAI`".to_string(),
                "historical harness evidence boundary".to_string(),
                "opencode deterministic harness reference".to_string(),
                "isolated project fixture boundary".to_string(),
                "fake provider control boundary".to_string(),
                "component session harness boundary".to_string(),
                "tool and permission harness boundary".to_string(),
                "server route harness boundary".to_string(),
                "subsystem split boundary".to_string(),
                "deterministic runtime evidence".to_string(),
                "Codex-style harness alignment".to_string(),
                "current `moyAI` opencode deterministic harness authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "opencode/packages/opencode/test/".to_string(),
                "test/fixture/fixture.ts".to_string(),
                "test/fake/provider.ts".to_string(),
                "test/lib/llm-server.ts".to_string(),
                "prompt-effect.test.ts".to_string(),
                "processor-effect.test.ts".to_string(),
                "compaction.test.ts".to_string(),
                "retry.test.ts".to_string(),
                "structured-output-integration.test.ts".to_string(),
                "session-actions.test.ts".to_string(),
                "permission-task.test.ts".to_string(),
                "task.test.ts".to_string(),
                "`apply_patch`".to_string(),
                "`write`".to_string(),
                "`bash`".to_string(),
                "`read`".to_string(),
                "SessionPrompt".to_string(),
                "SessionProcessor".to_string(),
                "case1".to_string(),
                "case3".to_string(),
                "case5".to_string(),
                "2026-04-".to_string(),
                "読む順番".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.roocode_verification_harness_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "RooCodeVerificationHarness current `moyAI` historical harness evidence boundary Roo Code stream harness reference CLI stream control-plane boundary tool contract harness boundary task runtime harness boundary completion and todo discipline dispatch guard boundary repetition guard harness boundary local-LLM recovery harness reference Codex-style harness alignment current `moyAI` Roo Code recovery harness authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "RooCodeVerificationHarness".to_string(),
                "current `moyAI`".to_string(),
                "historical harness evidence boundary".to_string(),
                "Roo Code stream harness reference".to_string(),
                "CLI stream control-plane boundary".to_string(),
                "tool contract harness boundary".to_string(),
                "task runtime harness boundary".to_string(),
                "completion and todo discipline".to_string(),
                "dispatch guard boundary".to_string(),
                "repetition guard harness boundary".to_string(),
                "local-LLM recovery harness reference".to_string(),
                "Codex-style harness alignment".to_string(),
                "current `moyAI` Roo Code recovery harness authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Roo-Code/".to_string(),
                "apps/cli/scripts".to_string(),
                "src/core/".to_string(),
                "stream-harness.ts".to_string(),
                "run.ts".to_string(),
                "attemptCompletionTool.spec.ts".to_string(),
                "updateTodoListTool.spec.ts".to_string(),
                "validateToolUse.spec.ts".to_string(),
                "ToolRepetitionDetector.spec.ts".to_string(),
                "Task.spec.ts".to_string(),
                "grace-retry-errors.spec.ts".to_string(),
                "followup-during-streaming.ts".to_string(),
                "followup-after-completion.ts".to_string(),
                "cancel-active-task.ts".to_string(),
                "start-while-busy.ts".to_string(),
                "multi-message-queue-order.ts".to_string(),
                "runStreamCase".to_string(),
                "stdin-prompt-stream".to_string(),
                "stream-json".to_string(),
                "`attempt_completion`".to_string(),
                "`update_todo_list`".to_string(),
                "<user_message>".to_string(),
                "`Task`".to_string(),
                "VS Code API mock".to_string(),
                "case5".to_string(),
                "2026-04-".to_string(),
                "LM Studio".to_string(),
                "project_sandbox".to_string(),
                "読む順番".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.survey.openclaw_runtime_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "OpenClawRuntimeSurvey current `moyAI` historical runtime evidence boundary OpenClaw tool lifecycle reference tool surface composition boundary execution contract boundary owned tool runtime boundary run-scoped loop detection boundary workspace file boundary exec lifecycle boundary Codex app-server adapter boundary projection isolation boundary Codex-style runtime alignment current `moyAI` OpenClaw runtime reference authority closed-network adoption boundary".to_string(),
            required_refs: vec![
                "OpenClawRuntimeSurvey".to_string(),
                "current `moyAI`".to_string(),
                "historical runtime evidence boundary".to_string(),
                "OpenClaw tool lifecycle reference".to_string(),
                "tool surface composition boundary".to_string(),
                "execution contract boundary".to_string(),
                "owned tool runtime boundary".to_string(),
                "run-scoped loop detection boundary".to_string(),
                "workspace file boundary".to_string(),
                "exec lifecycle boundary".to_string(),
                "Codex app-server adapter boundary".to_string(),
                "projection isolation boundary".to_string(),
                "Codex-style runtime alignment".to_string(),
                "current `moyAI` OpenClaw runtime reference authority".to_string(),
                "closed-network adoption boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "openclaw/".to_string(),
                "openclaw/src/".to_string(),
                "extensions/codex".to_string(),
                "package.json".to_string(),
                "openclaw.mjs".to_string(),
                "createOpenClawCodingTools".to_string(),
                "before_tool_call".to_string(),
                "after_tool_call".to_string(),
                "strict-agentic".to_string(),
                "`update_plan`".to_string(),
                "`runId`".to_string(),
                "@mariozechner".to_string(),
                "GPT-5".to_string(),
                "qwen".to_string(),
                "FR-080".to_string(),
                "case2".to_string(),
                "Case2LaneInvariantAudit".to_string(),
                "tool-loop-detection.ts".to_string(),
                "apply-patch.ts".to_string(),
                "bash-tools.exec-runtime.ts".to_string(),
                "run-attempt.ts".to_string(),
                "event-projector.ts".to_string(),
                "read/write/apply_patch".to_string(),
                "調査根拠として読んだ".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.item_lifecycle.provider_replay_call_output_symmetry"
                .to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "canonical_history_item_stream provider_replay provider replay canonical sequence order prompt_provider_replay_sequence_order stream_accumulator_complete_tool_call_lifecycle call_id_scoped_tool_output missing_output_aborted orphan_output_omitted runtime_error_excluded transcript_projection_display_only preflight_prompt_replay_fixture_current_provider_profile".to_string(),
            required_refs: vec![
                "canonical_history_item_stream".to_string(),
                "provider_replay".to_string(),
                "provider replay canonical sequence order".to_string(),
                "prompt_provider_replay_sequence_order".to_string(),
                "stream_accumulator_complete_tool_call_lifecycle".to_string(),
                "call_id_scoped_tool_output".to_string(),
                "missing_output_aborted".to_string(),
                "orphan_output_omitted".to_string(),
                "runtime_error_excluded".to_string(),
                "transcript_projection_display_only".to_string(),
                "preflight_prompt_replay_fixture_current_provider_profile".to_string(),
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
            authority_source: "ProviderStreamRetry stream_max_retries request_diagnostics_stream_max_retries stream_retry_exhausted_terminal_evidence sse_transport_error body_decode_error stream_idle_timeout retry_before_first_model_event no_retry_after_partial_model_event no_retry_for_parse_or_provider_error streaming_tool_call_late_name_typed_identity".to_string(),
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
                "streaming_tool_call_late_name_typed_identity".to_string(),
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
            authority_source: "TurnControlEnvelope ProjectionBundle ActionAuthority allowed_surface tool_choice availability_metadata satisfying_file_change_progress_surface explicit_required_action_conflict_fail_closed typed_required_action_obligation_state required_action_typed_projection_renderer unavailable_explicit_required_action_fail_closed projection_bundle_lifecycle_authority_alignment named_tool_choice_required_action_alignment action_authority_obligation_materialization active_contract_obligation_alignment verification_active_contract_obligation_alignment active_contract_context_lifecycle_alignment tool_surface_disjoint turn_decision_projection_alignment continuation_contract_alignment output_contract_obligation_alignment protocol_control_fixture_language_neutral protocol_runtime_fixture_current_provider_profile".to_string(),
            required_refs: vec![
                "TurnControlEnvelope".to_string(),
                "ProjectionBundle".to_string(),
                "ActionAuthority".to_string(),
                "availability_metadata".to_string(),
                "satisfying_file_change_progress_surface".to_string(),
                "explicit_required_action_conflict_fail_closed".to_string(),
                "typed_required_action_obligation_state".to_string(),
                "required_action_typed_projection_renderer".to_string(),
                "unavailable_explicit_required_action_fail_closed".to_string(),
                "projection_bundle_lifecycle_authority_alignment".to_string(),
                "named_tool_choice_required_action_alignment".to_string(),
                "action_authority_obligation_materialization".to_string(),
                "active_contract_obligation_alignment".to_string(),
                "verification_active_contract_obligation_alignment".to_string(),
                "active_contract_context_lifecycle_alignment".to_string(),
                "tool_surface_disjoint".to_string(),
                "turn_decision_projection_alignment".to_string(),
                "continuation_contract_alignment".to_string(),
                "output_contract_obligation_alignment".to_string(),
                "protocol_control_fixture_language_neutral".to_string(),
                "protocol_runtime_fixture_current_provider_profile".to_string(),
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
            authority_source: "StateAuthorityCandidate StateAuthorityDecision candidate_precedence single_owner_projection RequestedWorkAuthoring ReferenceInput reference_input_not_pending_deliverable scenario_contract_reference_input_not_authoring_target state_authoring_completion_no_progress_fixture_language_neutral state_requested_work_fixture_workflow_neutral state_residual_component_fixture_workflow_neutral state_authoring_verification_repair_transition_fixture_language_neutral state_new_authoring_turn_fixture_invariant_workspace_key docs_route_same_document_relative_workspace_fixture_language_neutral state_verification_docs_promotion_fixture_language_neutral state_metadata_only_filechange_fixture_language_neutral state_history_item_sequence_primary_order canonical_history_item_sequence_primary state_handoff_remaining_exact_target_identity state_blocked_reason_exact_target_identity state_reference_design_verification_fixture_language_neutral state_docs_output_verification_repair_fixture_language_neutral japanese_prompt_filename_token_boundary docs_output_referenced_code_not_pending_deliverable structured_document_summary_remaining_sources_block_closeout structured_document_summary_output_heading_progress_after_compaction state_structured_document_output_progress_exact_target_identity canonical_filechange_output_target_identity state_structured_document_docling_progress_exact_target_identity canonical_docling_source_target_identity state_structured_document_summary_generated_dependency_exclusion admissible_workspace_structured_document_targets no_verification_requested_work_file_change_closeout metadata_only_tool_output_not_filechange_authority generic_scaffold_stub_completion_guard dotted_technology_token_not_file_target todo_completion_kind_only_open_work_authority relative_workspace_root_absolute_filechange_progress escaped_windows_absolute_filechange_progress FileChangeEvidence authoring_complete Verification verification_command_obligation before_closeout canonical_item_chronology turn_local_sequence_no latest_content_change_invalidates_prior_verification VerificationRunResult passed_verification_command_consumed clean_closeout".to_string(),
            required_refs: vec![
                "StateAuthorityCandidate".to_string(),
                "StateAuthorityDecision".to_string(),
                "candidate_precedence".to_string(),
                "single_owner_projection".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "ReferenceInput".to_string(),
                "reference_input_not_pending_deliverable".to_string(),
                "scenario_contract_reference_input_not_authoring_target".to_string(),
                "state_authoring_completion_no_progress_fixture_language_neutral".to_string(),
                "state_requested_work_fixture_workflow_neutral".to_string(),
                "state_residual_component_fixture_workflow_neutral".to_string(),
                "state_authoring_verification_repair_transition_fixture_language_neutral"
                    .to_string(),
                "state_new_authoring_turn_fixture_invariant_workspace_key".to_string(),
                "docs_route_same_document_relative_workspace_fixture_language_neutral".to_string(),
                "state_verification_docs_promotion_fixture_language_neutral".to_string(),
                "state_metadata_only_filechange_fixture_language_neutral".to_string(),
                "state_history_item_sequence_primary_order".to_string(),
                "canonical_history_item_sequence_primary".to_string(),
                "state_handoff_remaining_exact_target_identity".to_string(),
                "state_blocked_reason_exact_target_identity".to_string(),
                "state_reference_design_verification_fixture_language_neutral".to_string(),
                "state_docs_output_verification_repair_fixture_language_neutral".to_string(),
                "japanese_prompt_filename_token_boundary".to_string(),
                "docs_output_referenced_code_not_pending_deliverable".to_string(),
                "structured_document_summary_remaining_sources_block_closeout".to_string(),
                "structured_document_summary_output_heading_progress_after_compaction".to_string(),
                "state_structured_document_output_progress_exact_target_identity".to_string(),
                "canonical_filechange_output_target_identity".to_string(),
                "state_structured_document_docling_progress_exact_target_identity".to_string(),
                "canonical_docling_source_target_identity".to_string(),
                "state_structured_document_summary_generated_dependency_exclusion".to_string(),
                "admissible_workspace_structured_document_targets".to_string(),
                "no_verification_requested_work_file_change_closeout".to_string(),
                "metadata_only_tool_output_not_filechange_authority".to_string(),
                "generic_scaffold_stub_completion_guard".to_string(),
                "dotted_technology_token_not_file_target".to_string(),
                "todo_completion_kind_only_open_work_authority".to_string(),
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
            authority_source: "RepairControlSnapshot FileChangeEvidence content_changing_repair_progress runner_byproduct_filechange_not_repair_progress target_normalization source_owned_generated_test_evidence_cleanup state_post_repair_verification_transition_fixture_language_neutral Verification exact_shell_rerun runtime_owned_required_verification_dispatch before_repair_reissue".to_string(),
            required_refs: vec![
                "RepairControlSnapshot".to_string(),
                "FileChangeEvidence".to_string(),
                "content_changing_repair_progress".to_string(),
                "runner_byproduct_filechange_not_repair_progress".to_string(),
                "target_normalization".to_string(),
                "source_owned_generated_test_evidence_cleanup".to_string(),
                "state_post_repair_verification_transition_fixture_language_neutral".to_string(),
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
            authority_source: "CodexHistoryItemStream session_state_projection_not_sequence_floor VerificationRunResult VerificationFailureCluster VerificationFailureEvidence public_output_stream_assertion_mismatch public_command_contract_failure_projection active_obligation_targets continuation_byproduct_path_not_repair_authority state_verification_repair_continuation_fixture_language_neutral state_verification_repair_byproduct_fixture_toolchain_neutral state_public_command_continuation_fixture_language_neutral state_public_command_continuation_summary_typed_observation state_generated_test_parse_continuation_fixture_language_neutral state_residual_component_fixture_workflow_neutral state_verification_docs_promotion_fixture_language_neutral state_docs_output_verification_repair_fixture_language_neutral state_authoring_verification_repair_transition_fixture_language_neutral state_source_generated_repair_authority_fixture_language_neutral state_verification_repair_target_fixture_workflow_neutral repair_lane_source_owned_fixture_language_neutral repair_lane_adjacent_authority_fixture_language_neutral repair_lane_generated_test_fixture_language_neutral repair_lane_public_command_generated_overreach_fixture_language_neutral repair_lane_source_target_identity_exact repair_lane_public_state_obligations_domain_neutral verification_repair_common_authority_predicate code_like_test_source_projection source_config_repair_target_authority source_config_repair_lane_authority language_neutral_source_call_site_repair_authority source_call_site_line_column_coordinate_authority source_owned_repair_control_snapshot source_owned_recent_file_change_target_preserved source_owned_repair_test_to_source_target_normalization source_owned_active_work_exact_target_projection source_owned_public_output_stream_active_work_exact_target_projection source_owned_requirement_refs_align_active_work_with_repair_lane stale_docs_route_not_verification_owner docs_reference_context_not_docs_owner contract_visible_public_exception_owner_authority generated_test_constructor_api_misuse_owner_authority generated_test_parse_defect_owner_authority generated_test_reflection_api_misuse_owner_authority generated_test_module_attribute_api_misuse_owner_authority generated_test_exception_type_overreach_owner_authority state_generated_test_exception_overreach_fixture_domain_neutral source_parse_defect_owner_authority generated_test_name_resolution_owner_authority generated_test_import_nameerror_owner_authority mixed_source_test_contract_reconciliation_owner_authority generated_test_contract_overreach_owner_projection_alignment generic_generated_test_only_owner_target_authority ungrounded_generated_public_output_assertion_owner_authority generated_test_local_binding_contradiction_owner_authority generated_test_local_binding_enrichment_exact_target_identity source_constructor_mismatch_counterexample verification_timeout_recent_source_target_preserved targetless_unclassified_repair_dispatch_blocked verification_labels_not_file_targets language_runtime_traceback_frames_excluded import_error_module_target_authority diagnostic_scalar_values_are_not_repair_targets source_only_contract_profile_no_synthetic_generated_test_target contract_reconciliation_target_identity_exact contract_reconciliation_cluster_refs_exact_target_identity tool_orchestrator_target_matching_exact_path_authority".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "session_state_projection_not_sequence_floor".to_string(),
                "VerificationRunResult".to_string(),
                "VerificationFailureCluster".to_string(),
                "VerificationFailureEvidence".to_string(),
                "public_output_stream_assertion_mismatch".to_string(),
                "public_command_contract_failure_projection".to_string(),
                "active_obligation_targets".to_string(),
                "continuation_byproduct_path_not_repair_authority".to_string(),
                "state_verification_repair_continuation_fixture_language_neutral".to_string(),
                "state_verification_repair_byproduct_fixture_toolchain_neutral".to_string(),
                "state_public_command_continuation_fixture_language_neutral".to_string(),
                "state_public_command_continuation_summary_typed_observation".to_string(),
                "state_generated_test_parse_continuation_fixture_language_neutral".to_string(),
                "state_residual_component_fixture_workflow_neutral".to_string(),
                "state_verification_docs_promotion_fixture_language_neutral".to_string(),
                "state_docs_output_verification_repair_fixture_language_neutral".to_string(),
                "state_authoring_verification_repair_transition_fixture_language_neutral"
                    .to_string(),
                "state_source_generated_repair_authority_fixture_language_neutral".to_string(),
                "state_verification_repair_target_fixture_workflow_neutral".to_string(),
                "repair_lane_source_owned_fixture_language_neutral".to_string(),
                "repair_lane_adjacent_authority_fixture_language_neutral".to_string(),
                "repair_lane_generated_test_fixture_language_neutral".to_string(),
                "repair_lane_public_command_generated_overreach_fixture_language_neutral"
                    .to_string(),
                "repair_lane_source_target_identity_exact".to_string(),
                "repair_lane_public_state_obligations_domain_neutral".to_string(),
                "verification_repair_common_authority_predicate".to_string(),
                "code_like_test_source_projection".to_string(),
                "source_config_repair_target_authority".to_string(),
                "source_config_repair_lane_authority".to_string(),
                "language_neutral_source_call_site_repair_authority".to_string(),
                "source_call_site_line_column_coordinate_authority".to_string(),
                "source_owned_repair_control_snapshot".to_string(),
                "source_owned_recent_file_change_target_preserved".to_string(),
                "source_owned_repair_test_to_source_target_normalization".to_string(),
                "source_owned_active_work_exact_target_projection".to_string(),
                "source_owned_public_output_stream_active_work_exact_target_projection"
                    .to_string(),
                "source_owned_requirement_refs_align_active_work_with_repair_lane"
                    .to_string(),
                "stale_docs_route_not_verification_owner".to_string(),
                "docs_reference_context_not_docs_owner".to_string(),
                "contract_visible_public_exception_owner_authority".to_string(),
                "generated_test_constructor_api_misuse_owner_authority".to_string(),
                "generated_test_parse_defect_owner_authority".to_string(),
                "generated_test_reflection_api_misuse_owner_authority".to_string(),
                "generated_test_module_attribute_api_misuse_owner_authority".to_string(),
                "generated_test_exception_type_overreach_owner_authority".to_string(),
                "state_generated_test_exception_overreach_fixture_domain_neutral".to_string(),
                "source_parse_defect_owner_authority".to_string(),
                "generated_test_contract_overreach_owner_projection_alignment".to_string(),
                "generic_generated_test_only_owner_target_authority".to_string(),
                "ungrounded_generated_public_output_assertion_owner_authority".to_string(),
                "generated_test_name_resolution_owner_authority".to_string(),
                "generated_test_import_nameerror_owner_authority".to_string(),
                "mixed_source_test_contract_reconciliation_owner_authority".to_string(),
                "generated_test_local_binding_contradiction_owner_authority".to_string(),
                "generated_test_local_binding_enrichment_exact_target_identity".to_string(),
                "source_constructor_mismatch_counterexample".to_string(),
                "verification_timeout_recent_source_target_preserved".to_string(),
                "targetless_unclassified_repair_dispatch_blocked".to_string(),
                "verification_labels_not_file_targets".to_string(),
                "language_runtime_traceback_frames_excluded".to_string(),
                "import_error_module_target_authority".to_string(),
                "diagnostic_scalar_values_are_not_repair_targets".to_string(),
                "source_only_contract_profile_no_synthetic_generated_test_target".to_string(),
                "contract_reconciliation_target_identity_exact".to_string(),
                "contract_reconciliation_cluster_refs_exact_target_identity".to_string(),
                if crate::agent::tool_orchestrator::tool_orchestrator_target_matching_exact_path_authority_fixture_passes() {
                    "tool_orchestrator_target_matching_exact_path_authority".to_string()
                } else {
                    "tool_orchestrator_target_matching_exact_path_authority_failed".to_string()
                },
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
                "existing_workspace_path_as_repair_authority".to_string(),
                "local_code_test_docs_only_repair_authority_predicate".to_string(),
                "python_suffix_as_source_ownership_authority".to_string(),
                "latest_user_text_containment_as_docs_owner".to_string(),
                "synthetic_generated_test_target_from_source_filename".to_string(),
                "sibling_root_suffix_repair_target_authority".to_string(),
                "game_loop_public_state_obligation_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.state_reducer.docs_route_contract_authority".to_string(),
            family: PreflightGateFamily::StateReducerAuthority,
            authority_source: "CodexHistoryItemStream TaskRoute::Docs DocsRouteState DocsRepair route_contract_pending route_contract_satisfied_typed_closeout docs_only_mutation_boundary docs_route_closeout_continuation_preserves_docs_authority state_docs_closeout_continuation_exact_target_identity docs_route_verification_failure_preserves_docs_active_target docs_route_verification_target_fixture_language_neutral docs_route_repair_lane_target_authority same_document_docs_update_route_authority same_document_update_requires_latest_file_change prior_authored_document_update_target docs_route_same_document_relative_workspace_fixture_language_neutral state_docs_route_target_alias_identity_exact state_docs_route_fixture_workflow_neutral dynamic_docs_area_contract flat_test_artifact_area_coverage generated_dependency_evidence_excluded RequestedWorkAuthoring_not_primary localized_docs_topic_completion docs_route_semantic_no_progress_guard docs_spec_semantic_reconciliation_no_progress_terminal_guard docs_supporting_context_budget_exhausted_corrective_tool_output docs_budget_exhausted_recovery_surface_narrowed docs_budget_exhaustion_survives_partial_write loop_impl_docs_budget_edit_surface_fixture_language_neutral loop_impl_docs_route_budget_fixture_workflow_neutral loop_impl_active_authoring_docs_regression_fixture_domain_neutral loop_impl_docs_existing_target_grounding_fixture_domain_neutral supporting_context_evidence_survives_surface_narrowing docs_content_grounding_before_exact_write_recovery docs_required_topic_content_grounding docs_completed_deliverable_regression_rejected write_ready_prompt_projection docs_spec_semantic_reconciliation".to_string(),
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
                "docs_route_same_document_relative_workspace_fixture_language_neutral".to_string(),
                "state_docs_route_target_alias_identity_exact".to_string(),
                "state_docs_route_fixture_workflow_neutral".to_string(),
                "flat_test_artifact_area_coverage".to_string(),
                "generated_dependency_evidence_excluded".to_string(),
                "localized_docs_topic_completion".to_string(),
                "docs_route_semantic_no_progress_guard".to_string(),
                "docs_route_closeout_continuation_preserves_docs_authority".to_string(),
                "state_docs_closeout_continuation_exact_target_identity".to_string(),
                "docs_route_verification_failure_preserves_docs_active_target".to_string(),
                "docs_route_verification_target_fixture_language_neutral".to_string(),
                "docs_route_repair_lane_target_authority".to_string(),
                "docs_spec_semantic_reconciliation_no_progress_terminal_guard".to_string(),
                "docs_supporting_context_budget_exhausted_corrective_tool_output".to_string(),
                "docs_budget_exhausted_recovery_surface_narrowed".to_string(),
                "docs_budget_exhaustion_survives_partial_write".to_string(),
                "loop_impl_docs_budget_edit_surface_fixture_language_neutral".to_string(),
                "loop_impl_docs_route_budget_fixture_workflow_neutral".to_string(),
                "loop_impl_active_authoring_docs_regression_fixture_domain_neutral".to_string(),
                "loop_impl_docs_existing_target_grounding_fixture_domain_neutral".to_string(),
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
            authority_source: "DocsSpecSemanticContract latest_user_request_authority required_claims prohibited_claims semantic_reconciliation_before_handoff semantic_cli_subject_shape_projection markdown_heading_context_claim_segments unknown_two_token_evidence_context_bounded side_effect_free_corrective_tool_output no_file_change_progress_on_contradiction prompt_visible_reconciliation_contract docs_semantic_reconciliation_feedback_projection docs_semantic_contract_fixture_workflow_neutral docs_semantic_contract_cli_fixture_workflow_neutral docs_semantic_claim_projection_operation_authority docs_semantic_documentation_target_classifier_shape_based docs_semantic_prohibited_claim_requires_affirmative_occurrence docs_spec_semantic_reconciliation_no_progress_terminal_guard semantic_claim_key_payload_independent".to_string(),
            required_refs: vec![
                "DocsSpecSemanticContract".to_string(),
                "latest_user_request_authority".to_string(),
                "required_claims".to_string(),
                "prohibited_claims".to_string(),
                "semantic_reconciliation_before_handoff".to_string(),
                "semantic_cli_subject_shape_projection".to_string(),
                "markdown_heading_context_claim_segments".to_string(),
                "unknown_two_token_evidence_context_bounded".to_string(),
                "side_effect_free_corrective_tool_output".to_string(),
                "no_file_change_progress_on_contradiction".to_string(),
                "prompt_visible_reconciliation_contract".to_string(),
                "docs_semantic_reconciliation_feedback_projection".to_string(),
                "docs_semantic_contract_fixture_workflow_neutral".to_string(),
                "docs_semantic_contract_cli_fixture_workflow_neutral".to_string(),
                "docs_semantic_claim_projection_operation_authority".to_string(),
                "docs_semantic_documentation_target_classifier_shape_based".to_string(),
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
            authority_source: "PublicCommandContract prompt_visible_command_examples generic_public_command_obligation public_command_bare_span_execution_context_boundary generic_test_artifact_public_command_evidence generated_test_subprocess_coverage generic_child_process_execution_contract generic_child_process_timeout_feedback argv_tokens expected_exit_code stdout_stderr_observation stdout_line_suffix_delimited_suffix route_verification_evidence route_verification_utf8_process_environment expected_nonzero_pass_allowed delta_edit_post_patch_candidate_projection parent_child_encoding_alignment subprocess_timeout_authority subprocess_output_capture_authority public_command_contract_issue_kind public_command_contract_failure_projection typed_missing_coverage_feedback public_command_observation_assertion_templates compact_route_failure_evidence source_public_command_contract encoding_contract_issues subprocess_timeout_contract_issues subprocess_output_capture_contract_issues public_command_contract_fixture_workflow_neutral loop_impl_verification_public_command_fixture_domain_neutral".to_string(),
            required_refs: vec![
                "PublicCommandContract".to_string(),
                "prompt_visible_command_examples".to_string(),
                "generic_public_command_obligation".to_string(),
                "public_command_bare_span_execution_context_boundary".to_string(),
                "generic_test_artifact_public_command_evidence".to_string(),
                "generated_test_subprocess_coverage".to_string(),
                "generic_child_process_execution_contract".to_string(),
                "generic_child_process_timeout_feedback".to_string(),
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
                "typed_missing_coverage_feedback".to_string(),
                "public_command_observation_assertion_templates".to_string(),
                "source_public_command_contract".to_string(),
                "encoding_contract_issues".to_string(),
                "subprocess_timeout_contract_issues".to_string(),
                "subprocess_output_capture_contract_issues".to_string(),
                "public_command_contract_fixture_workflow_neutral".to_string(),
                "loop_impl_verification_public_command_fixture_domain_neutral".to_string(),
            ],
            forbidden_refs: vec![
                "case3_primary_key".to_string(),
                "calculator_primary_key".to_string(),
                "calculator_public_command_fixture_authority".to_string(),
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
            authority_source: "VerificationRunResult original_command effective_command satisfies_command_identities command_correction_alias required_verification_obligation_consumption command_identity_deduplication generic_build_check_requirement verification_dotted_technology_token_not_file_target state_verification_command_identity_fixture_workflow_neutral".to_string(),
            required_refs: vec![
                "VerificationRunResult".to_string(),
                "satisfies_command_identities".to_string(),
                "command_correction_alias".to_string(),
                "required_verification_obligation_consumption".to_string(),
                "command_identity_deduplication".to_string(),
                "generic_build_check_requirement".to_string(),
                "verification_dotted_technology_token_not_file_target".to_string(),
                "state_verification_command_identity_fixture_workflow_neutral".to_string(),
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
            authority_source: "CodexPlanTool ProgressProjection RequestedWorkAuthoring write_authority todo_absence_nonblocking progress_projection_not_work_progress progress_side_channel_not_first_artifact_action todowrite_progress_projection_fixture_language_neutral open_work_terminal_guard".to_string(),
            required_refs: vec![
                "CodexPlanTool".to_string(),
                "ProgressProjection".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "write_authority".to_string(),
                "todo_absence_nonblocking".to_string(),
                "progress_projection_not_work_progress".to_string(),
                "progress_side_channel_not_first_artifact_action".to_string(),
                "todowrite_progress_projection_fixture_language_neutral".to_string(),
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
            authority_source: "CanonicalHistoryItem PromptProjection current_lifecycle_state current_todo_focus_projection typed_tool_output_feedback_prompt_authority runtime_input_history_item_only_authority typed_verification_run_prompt_cycle_authority typed_verification_run_prompt_label_authority typed_rejected_tool_proposal_prompt_authority typed_patch_recovery_state_authority typed_pseudo_tool_rejection_prompt_authority typed_code_block_stall_history_authority typed_superseded_tool_denial_history_authority typed_compaction_replay_history_authority typed_prompt_projection_workspace_root_authority typed_continuation_expected_artifacts_evidence_contract typed_requested_work_section_role_contract typed_docs_audit_metadata_prompt_authority message_only_history_not_lifecycle_authority verification_repair_read_budget_typed_history_authority verification_repair_target_rotation_typed_history_authority prompt_projection_fixture_domain_neutral prompt_verification_repair_fixture_language_neutral prompt_content_shape_window_fixture_workflow_neutral prompt_docs_followup_heuristic_domain_neutral prompt_provider_replay_residual_fixture_workflow_neutral prompt_staged_task_target_identity_exact prompt_provider_replay_inactive_filechange_exact_target_identity prompt_assets_staged_docs_deliverable_projection_workflow_neutral prompt_assets_documentation_target_classifier_shape_based prompt_assets_python_context_uses_language_evidence_adapter prompt_assets_contract_guidance_typed_verification_evidence verification_evidence_typed_history_authority staged_task_closeout_repair_typed_history_authority staged_task_recovery_stall_typed_history_authority staged_task_output_lifecycle_typed_history_authority documentation_prompt_lifecycle_typed_history_authority follow_up_focus_typed_history_authority language_evidence_adapter_registry language_evidence_fixture_workflow_neutral prompt_assets_fixture_workflow_neutral prompt_fixture_workflow_neutral prompt_residual_fixture_workflow_neutral prompt_artifact_target_kind_uses_adapter verification_repair_prompt_language_projection generic_language_verification_command_registry generic_verification_command_kind_split generic_verification_artifact_role_stem language_failure_label_adapter prompt_content_shape_projection_uses_adapter_contract generated_test_shape_language_adapter_owner_contract python_source_shape_language_adapter_owner_contract content_shape_language_adapter_consumer_surface write_content_test_contract stable_provider_tool_schema provider_owned_tool_arguments final_dispatch_source_schema_projection exact_write_path_schema_projection positive_test_module_shape_contract executable_test_module_shape_contract test_class_base_contract subprocess_returncode_diagnostics_contract module_qualified_reference_import_contract test_runner_self_invocation_rejected string_literal_test_module_rejected source_executable_artifact_shape_contract generic_code_artifact_effective_content_shape content_shape_fixture_workflow_neutral loop_impl_escaped_source_fixture_language_neutral source_required_public_surface_allowed source_boolean_comparison_continuation_allowed source_duplicate_entrypoint_rejected shell_file_change_content_shape_violation_rejected executed_content_shape_feedback_kind escaped_source_string_rejected escaped_source_write_candidate_normalized generic_escaped_source_repair_ref_projection source_test_module_payload_rejected corrective_content_shape_no_progress_terminal_guard python_source_repair_positive_contract text_artifact_readable_content_shape serialized_markdown_rejected text_artifact_repair_positive_contract content_shape_workspace_target_normalization consumed_supporting_context_replay_omitted post_patch_test_module_shape_contract observed_forbidden_marker_feedback unittest_main_test_content_allowed required_write_content_shape_typed_progress_class content_shape_mismatch_current_action_projection sanitized_failed_write_tool_call_lifecycle summary_only_tool_call_replay omitted_corrective_output_latest_recovery stale_arguments_suppressed stale_payload_omitted stale_prelude_omitted stale_todo_progress_replay_omitted internal_control_items_not_provider_visible preflight_prompt_replay_fixture_current_provider_profile".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "current_lifecycle_state".to_string(),
                "current_todo_focus_projection".to_string(),
                "typed_tool_output_feedback_prompt_authority".to_string(),
                "runtime_input_history_item_only_authority".to_string(),
                "typed_verification_run_prompt_cycle_authority".to_string(),
                "typed_verification_run_prompt_label_authority".to_string(),
                "typed_rejected_tool_proposal_prompt_authority".to_string(),
                "typed_patch_recovery_state_authority".to_string(),
                "typed_pseudo_tool_rejection_prompt_authority".to_string(),
                "typed_code_block_stall_history_authority".to_string(),
                "typed_superseded_tool_denial_history_authority".to_string(),
                "typed_compaction_replay_history_authority".to_string(),
                "typed_prompt_projection_workspace_root_authority".to_string(),
                "typed_continuation_expected_artifacts_evidence_contract".to_string(),
                "typed_requested_work_section_role_contract".to_string(),
                "typed_docs_audit_metadata_prompt_authority".to_string(),
                "message_only_history_not_lifecycle_authority".to_string(),
                "verification_repair_read_budget_typed_history_authority".to_string(),
                "verification_repair_target_rotation_typed_history_authority".to_string(),
                "prompt_projection_fixture_domain_neutral".to_string(),
                "prompt_verification_repair_fixture_language_neutral".to_string(),
                "prompt_content_shape_window_fixture_workflow_neutral".to_string(),
                "prompt_docs_followup_heuristic_domain_neutral".to_string(),
                "prompt_provider_replay_residual_fixture_workflow_neutral".to_string(),
                "prompt_staged_task_target_identity_exact".to_string(),
                "prompt_provider_replay_inactive_filechange_exact_target_identity".to_string(),
                "prompt_assets_staged_docs_deliverable_projection_workflow_neutral".to_string(),
                "prompt_assets_documentation_target_classifier_shape_based".to_string(),
                "prompt_assets_python_context_uses_language_evidence_adapter".to_string(),
                "prompt_assets_contract_guidance_typed_verification_evidence".to_string(),
                "verification_evidence_typed_history_authority".to_string(),
                "staged_task_closeout_repair_typed_history_authority".to_string(),
                "staged_task_recovery_stall_typed_history_authority".to_string(),
                "staged_task_output_lifecycle_typed_history_authority".to_string(),
                "documentation_prompt_lifecycle_typed_history_authority".to_string(),
                "follow_up_focus_typed_history_authority".to_string(),
                "language_evidence_adapter_registry".to_string(),
                "language_evidence_fixture_workflow_neutral".to_string(),
                "prompt_assets_fixture_workflow_neutral".to_string(),
                "prompt_fixture_workflow_neutral".to_string(),
                "prompt_residual_fixture_workflow_neutral".to_string(),
                "prompt_artifact_target_kind_uses_adapter".to_string(),
                "verification_repair_prompt_language_projection".to_string(),
                "generic_language_verification_command_registry".to_string(),
                "generic_verification_command_kind_split".to_string(),
                "generic_verification_artifact_role_stem".to_string(),
                "language_failure_label_adapter".to_string(),
                "prompt_content_shape_projection_uses_adapter_contract".to_string(),
                "generated_test_shape_language_adapter_owner_contract".to_string(),
                "python_source_shape_language_adapter_owner_contract".to_string(),
                "content_shape_language_adapter_consumer_surface".to_string(),
                "write_content_test_contract".to_string(),
                "stable_provider_tool_schema".to_string(),
                "final_dispatch_source_schema_projection".to_string(),
                "exact_write_path_schema_projection".to_string(),
                "summary_only_tool_call_replay".to_string(),
                "provider_owned_tool_arguments".to_string(),
                "positive_test_module_shape_contract".to_string(),
                "executable_test_module_shape_contract".to_string(),
                "test_class_base_contract".to_string(),
                "subprocess_returncode_diagnostics_contract".to_string(),
                "module_qualified_reference_import_contract".to_string(),
                "test_runner_self_invocation_rejected".to_string(),
                "string_literal_test_module_rejected".to_string(),
                "source_executable_artifact_shape_contract".to_string(),
                "generic_code_artifact_effective_content_shape".to_string(),
                "content_shape_fixture_workflow_neutral".to_string(),
                "loop_impl_escaped_source_fixture_language_neutral".to_string(),
                "source_required_public_surface_allowed".to_string(),
                "source_boolean_comparison_continuation_allowed".to_string(),
                "source_duplicate_entrypoint_rejected".to_string(),
                "shell_file_change_content_shape_violation_rejected".to_string(),
                "executed_content_shape_feedback_kind".to_string(),
                "escaped_source_string_rejected".to_string(),
                "escaped_source_write_candidate_normalized".to_string(),
                "generic_escaped_source_repair_ref_projection".to_string(),
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
                "required_write_content_shape_typed_progress_class".to_string(),
                "content_shape_mismatch_current_action_projection".to_string(),
                "sanitized_failed_write_tool_call_lifecycle".to_string(),
                "omitted_corrective_output_latest_recovery".to_string(),
                "stale_arguments_suppressed".to_string(),
                "stale_payload_omitted".to_string(),
                "stale_prelude_omitted".to_string(),
                "stale_todo_progress_replay_omitted".to_string(),
                "internal_control_items_not_provider_visible".to_string(),
                "preflight_prompt_replay_fixture_current_provider_profile".to_string(),
            ],
            forbidden_refs: vec![
                "stale_write_arguments_authority".to_string(),
                "todowrite_history_as_action_authority".to_string(),
                "component_widget_fixture_authority".to_string(),
                "component_manual_fixture_authority".to_string(),
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
            authority_source: "CanonicalHistoryItem PromptProjection call_id_scoped_tool_call_output_pair model_arguments_replay_authority effective_tool_surface_scoped_replay loop_impl_provider_replay_effective_surface_fixture_effective_test_payload typed_supporting_context_replay_signal loop_impl_provider_replay_supporting_context_fixture_language_neutral lifecycle_kernel_fixture_workflow_neutral untyped_successful_read_not_consumed_supporting_context typed_provider_noncompliance_replay_signal metadata_backed_tool_feedback_replay_signal metadata_backed_content_shape_replay_feedback untyped_content_shape_title_not_lifecycle_authority metadata_backed_invalid_edit_arguments_replay_feedback untyped_invalid_edit_title_not_lifecycle_authority intermediate_assistant_text_omitted_while_open assistant_tool_call_content_stripped_while_open rejected_final_message_no_progress_evidence current_malformed_edit_arguments_sanitized invalid_edit_arguments_output_preserved no_orphan_tool_output latest_user_input_preserved".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "call_id_scoped_tool_call_output_pair".to_string(),
                "model_arguments_replay_authority".to_string(),
                "effective_tool_surface_scoped_replay".to_string(),
                "loop_impl_provider_replay_effective_surface_fixture_effective_test_payload"
                    .to_string(),
                "typed_supporting_context_replay_signal".to_string(),
                "loop_impl_provider_replay_supporting_context_fixture_language_neutral".to_string(),
                "lifecycle_kernel_fixture_workflow_neutral".to_string(),
                "untyped_successful_read_not_consumed_supporting_context".to_string(),
                "typed_provider_noncompliance_replay_signal".to_string(),
                "metadata_backed_tool_feedback_replay_signal".to_string(),
                "metadata_backed_content_shape_replay_feedback".to_string(),
                "untyped_content_shape_title_not_lifecycle_authority".to_string(),
                "metadata_backed_invalid_edit_arguments_replay_feedback".to_string(),
                "untyped_invalid_edit_title_not_lifecycle_authority".to_string(),
                "intermediate_assistant_text_omitted_while_open".to_string(),
                "assistant_tool_call_content_stripped_while_open".to_string(),
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
                "plain_grounding_prose_as_supporting_context".to_string(),
                "plain_provider_noncompliance_prose_as_corrective_output".to_string(),
                "spoofed_tool_feedback_text_as_lifecycle_authority".to_string(),
                "content_shape_title_as_lifecycle_authority".to_string(),
                "invalid_edit_title_as_lifecycle_authority".to_string(),
                "unaccepted_assistant_text_as_completion_authority".to_string(),
                "assistant_tool_call_content_as_completion_authority".to_string(),
                "rejected_final_message_item_omitted".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.compaction_orphan_assistant_repaired".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection CompactionContinuity LifecycleGuardSnapshot lifecycle_guard_snapshot_continuity lifecycle_guard_snapshot_canonical_history_order compaction_sequence_order_workflow_neutral model_claimed_continuity_not_authority typed_continuation_contract_before_summary post_compaction_role_alternation latest_user_input_preserved matching_user_query_restored_before_assistant compaction_trigger_ignores_pre_summary_history compaction_trigger_canonical_history_order content_shape_repair_history_window_authority provider_replay_canonical_history_order".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "CompactionContinuity".to_string(),
                "LifecycleGuardSnapshot".to_string(),
                "lifecycle_guard_snapshot_continuity".to_string(),
                "lifecycle_guard_snapshot_canonical_history_order".to_string(),
                "compaction_sequence_order_workflow_neutral".to_string(),
                "model_claimed_continuity_not_authority".to_string(),
                "typed_continuation_contract_before_summary".to_string(),
                "post_compaction_role_alternation".to_string(),
                "latest_user_input_preserved".to_string(),
                "matching_user_query_restored_before_assistant".to_string(),
                "compaction_trigger_ignores_pre_summary_history".to_string(),
                "compaction_trigger_canonical_history_order".to_string(),
                "content_shape_repair_history_window_authority".to_string(),
                "provider_replay_canonical_history_order".to_string(),
            ],
            forbidden_refs: vec![
                "assistant_message_without_matching_user_after_compaction".to_string(),
                "provider_template_no_user_query".to_string(),
                "pre_summary_history_token_pressure_retrigger".to_string(),
                "transcript_window_as_history_authority".to_string(),
                "raw_vector_order_as_replay_authority".to_string(),
                "raw_vector_order_as_compaction_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.stale_inactive_authoring_pair_omitted"
                .to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection stale_inactive_authoring_pair_omitted non_executable_history_summary reference_only_inactive_artifact_snapshot inactive_filechange_without_replayable_tool_call_snapshot metadata_only_tool_output_not_filechange_reference_snapshot no_fake_executable_tool_arguments current_active_target_preserved no_orphan_tool_output system_context_top_level_only wrong_target_tool_output_current_action_projection metadata_backed_wrong_target_replay_feedback untyped_wrong_target_title_not_lifecycle_authority edit_file_change_feedback_all_kinds_evidence_only".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "stale_inactive_authoring_pair_omitted".to_string(),
                "non_executable_history_summary".to_string(),
                "reference_only_inactive_artifact_snapshot".to_string(),
                "inactive_filechange_without_replayable_tool_call_snapshot".to_string(),
                "metadata_only_tool_output_not_filechange_reference_snapshot".to_string(),
                "no_fake_executable_tool_arguments".to_string(),
                "current_active_target_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
                "system_context_top_level_only".to_string(),
                "wrong_target_tool_output_current_action_projection".to_string(),
                "metadata_backed_wrong_target_replay_feedback".to_string(),
                "untyped_wrong_target_title_not_lifecycle_authority".to_string(),
                "edit_file_change_feedback_all_kinds_evidence_only".to_string(),
            ],
            forbidden_refs: vec![
                "[omitted inactive authoring target]".to_string(),
                "[omitted stale inactive authoring payload".to_string(),
                "sentinel_path_as_tool_argument".to_string(),
                "system_after_user_provider_message".to_string(),
                "wrong_target_title_as_lifecycle_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.prompt_replay.progress_projection_pair_omitted".to_string(),
            family: PreflightGateFamily::PromptReplayAuthority,
            authority_source: "CanonicalHistoryItem PromptProjection progress_projection_pair_omitted non_executable_planning_context current_progress_feedback_pair_preserved metadata_backed_progress_projection_feedback untyped_progress_projection_text_not_current_feedback call_id_scoped_current_plan_output no_stale_todo_json current_active_target_preserved no_orphan_tool_output".to_string(),
            required_refs: vec![
                "CanonicalHistoryItem".to_string(),
                "PromptProjection".to_string(),
                "progress_projection_pair_omitted".to_string(),
                "non_executable_planning_context".to_string(),
                "current_progress_feedback_pair_preserved".to_string(),
                "metadata_backed_progress_projection_feedback".to_string(),
                "untyped_progress_projection_text_not_current_feedback".to_string(),
                "call_id_scoped_current_plan_output".to_string(),
                "no_stale_todo_json".to_string(),
                "current_active_target_preserved".to_string(),
                "no_orphan_tool_output".to_string(),
            ],
            forbidden_refs: vec![
                "stale_todo_json_as_tool_argument".to_string(),
                "progress_projection_as_current_authoring_plan".to_string(),
                "progress_projection_text_as_current_feedback_authority".to_string(),
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
            authority_source: "CurrentProviderProfilePolicy PromptProjection language_policy no_thinking_policy tool_lifecycle_compatibility llm_provider_policy_tool_lifecycle_upgrade open_obligation_tool_authority final_assistant_after_obligations_only configured_tool_turn_output_budget llm_contract_fixture_language_neutral llm_contract_fixture_current_provider_profile openai_compat_fixture_language_neutral configured_max_output_tokens effective_max_output_tokens output_budget_reason openai_compatible_system_authority_merge single_leading_system_message runtime_system_control_projection_merge replay_metadata_not_provider_serialized replay_metadata_not_provider_deserialized provider_extra_body_reserved_key_guard request_diagnostics_runtime_tool_choice_projection request_diagnostics_chat_request_tool_surface request_diagnostics_model_capability_snapshot request_diagnostics_capability_absence_preserved request_diagnostics_parallel_tool_call_scope control_plane_parallel_tool_call_projection config_default_provider_profile_lm_studio provider_metadata_mode_default_lm_studio model_probe_fixture_provider_profile_openai_compatible model_probe_typed_arguments_schema_validation desktop_startup_fixture_current_provider_profile desktop_web_model_fixture_current_provider_profile_domain_neutral desktop_app_fixture_current_provider_profile model_availability_probe_shared_transport_projection model_availability_probe_tool_capability_hydration model_availability_probe_vision_capability_hydration runtime_model_availability_hydration prompt_enhance_model_availability_hydration desktop_startup_model_availability_report_gate desktop_image_dispatch_runtime_capability_gate desktop_model_summary_metadata_capability_evidence provider_neutral_typed_tool_choice typed_parallel_tool_call_projection parallel_tool_calls_false_projection no_tool_parallel_tool_calls_omitted chat_request_lifecycle_surface_validation chat_request_image_capability_validation chat_request_tool_capability_validation".to_string(),
            required_refs: vec![
                "CurrentProviderProfilePolicy".to_string(),
                "PromptProjection".to_string(),
                "language_policy".to_string(),
                "no_thinking_policy".to_string(),
                "tool_lifecycle_compatibility".to_string(),
                "llm_provider_policy_tool_lifecycle_upgrade".to_string(),
                "open_obligation_tool_authority".to_string(),
                "final_assistant_after_obligations_only".to_string(),
                "configured_tool_turn_output_budget".to_string(),
                "llm_contract_fixture_language_neutral".to_string(),
                "llm_contract_fixture_current_provider_profile".to_string(),
                "openai_compat_fixture_language_neutral".to_string(),
                "configured_max_output_tokens".to_string(),
                "effective_max_output_tokens".to_string(),
                "output_budget_reason".to_string(),
                "openai_compatible_system_authority_merge".to_string(),
                "single_leading_system_message".to_string(),
                "runtime_system_control_projection_merge".to_string(),
                "replay_metadata_not_provider_serialized".to_string(),
                "replay_metadata_not_provider_deserialized".to_string(),
                "provider_extra_body_reserved_key_guard".to_string(),
                "request_diagnostics_runtime_tool_choice_projection".to_string(),
                "request_diagnostics_chat_request_tool_surface".to_string(),
                "request_diagnostics_model_capability_snapshot".to_string(),
                "request_diagnostics_capability_absence_preserved".to_string(),
                "request_diagnostics_parallel_tool_call_scope".to_string(),
                "control_plane_parallel_tool_call_projection".to_string(),
                "config_default_provider_profile_lm_studio".to_string(),
                "provider_metadata_mode_default_lm_studio".to_string(),
                "model_probe_fixture_provider_profile_openai_compatible".to_string(),
                "model_probe_typed_arguments_schema_validation".to_string(),
                "desktop_startup_fixture_current_provider_profile".to_string(),
                "desktop_web_model_fixture_current_provider_profile_domain_neutral".to_string(),
                "desktop_app_fixture_current_provider_profile".to_string(),
                "model_availability_probe_shared_transport_projection".to_string(),
                "model_availability_probe_tool_capability_hydration".to_string(),
                "model_availability_probe_vision_capability_hydration".to_string(),
                "runtime_model_availability_hydration".to_string(),
                "prompt_enhance_model_availability_hydration".to_string(),
                "desktop_startup_model_availability_report_gate".to_string(),
                "desktop_image_dispatch_runtime_capability_gate".to_string(),
                "desktop_model_summary_metadata_capability_evidence".to_string(),
                "provider_neutral_typed_tool_choice".to_string(),
                "typed_parallel_tool_call_projection".to_string(),
                "parallel_tool_calls_false_projection".to_string(),
                "no_tool_parallel_tool_calls_omitted".to_string(),
                "chat_request_lifecycle_surface_validation".to_string(),
                "chat_request_image_capability_validation".to_string(),
                "chat_request_tool_capability_validation".to_string(),
            ],
            forbidden_refs: vec![
                "provider_policy_overrides_tool_lifecycle".to_string(),
                "final_answer_only_with_open_obligations".to_string(),
                "multiple_system_message_authority_roots".to_string(),
                "replay_metadata_in_provider_payload".to_string(),
                "replay_metadata_from_provider_payload".to_string(),
                "parallel_tool_calls_extra_body_authority".to_string(),
                "tool_choice_without_tool_surface".to_string(),
                "image_payload_without_vision_capability".to_string(),
                "tool_payload_without_tool_capability".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.lifecycle_kernel.turn_lifecycle_plan_authority".to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "TurnLifecycleKernel TurnLifecyclePlan TurnRuntime TurnControlEnvelope protocol_turn_id dispatch_tool_choice replay_policy proposal_policy corrective_policy terminal_policy continuation_expectation diagnostics_projection lifecycle_guard_state lifecycle_guard_snapshot_hydration lifecycle_guard_snapshot_canonical_history_order loop_impl_lifecycle_guard_hydration_sequence_order loop_impl_control_envelope_current_turn_id".to_string(),
            required_refs: vec![
                "TurnLifecycleKernel".to_string(),
                "TurnLifecyclePlan".to_string(),
                "TurnRuntime".to_string(),
                "TurnControlEnvelope".to_string(),
                "protocol_turn_id".to_string(),
                "dispatch_tool_choice".to_string(),
                "replay_policy".to_string(),
                "proposal_policy".to_string(),
                "corrective_policy".to_string(),
                "terminal_policy".to_string(),
                "continuation_expectation".to_string(),
                "diagnostics_projection".to_string(),
                "lifecycle_guard_state".to_string(),
                "lifecycle_guard_snapshot_hydration".to_string(),
                "lifecycle_guard_snapshot_canonical_history_order".to_string(),
                "loop_impl_lifecycle_guard_hydration_sequence_order".to_string(),
                "loop_impl_control_envelope_current_turn_id".to_string(),
            ],
            forbidden_refs: vec![
                "TurnRuntime_branch_policy".to_string(),
                "fresh_turn_id_in_control_envelope".to_string(),
                "timestamp_primary_lifecycle_guard_order".to_string(),
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
            authority_source: "ToolLifecycleEnvelope FunctionCallOutput success=false progress_effect=no_progress common_file_change_admission write_atomic_filechange_commit_transaction write_no_content_fixture_language_neutral apply_patch_operation_pre_admission apply_patch_move_destination_fresh_authority multi_path_edit_lock_set apply_patch_single_invocation_permission_admission apply_patch_permission_before_formatter_validation apply_patch_tool_invocation_lock_scope apply_patch_admitted_execution_plan apply_patch_unique_participant_ownership apply_patch_duplicate_participant_before_formatter_validation apply_patch_atomic_commit_transaction unambiguous_malformed_edit_argument_repair malformed_edit_argument_repair_projection empty_file_change_not_authoring_progress corrective_content_shape_requires_typed_progress_class invalid_edit_arguments_control_recovery_projection loop_impl_invalid_edit_fixture_language_neutral invalid_edit_recovery_candidate_target_operation invalid_edit_recovery_uses_open_target_when_candidate_is_inactive invalid_edit_recovery_candidate_target_normalized mixed_target_apply_patch_active_hunk_evidence mixed_target_invalid_edit_recovery_projection invalid_edit_arguments_recovery_persists_across_final_message invalid_apply_patch_required_action_surface_alignment edit_recovery_fixture_workflow_neutral loop_impl_invalid_edit_failed_edit_recovery_fixture_language_neutral tool_orchestrator_fixture_language_neutral parser_error raw_argument_shape_hash allowed_surface_snapshot malformed_write_patch_capable_recovery_surface loop_impl_malformed_write_fixture_language_neutral malformed_apply_patch_patch_capable_recovery_surface loop_impl_malformed_apply_patch_fixture_language_neutral active_generated_test_malformed_apply_patch_required_action_surface_alignment stale_inactive_malformed_apply_patch_stays_no_write_recovery no_content_change idempotent_file_write_no_progress idempotent_apply_patch_no_progress idempotent_file_write_terminal_guard destructive_noop_acknowledgement_patch_rejected empty_apply_patch_hunks_rejected hunkless_update_patch_rejected bare_markdown_update_body_rejected add_file_unprefixed_content_line_feedback edit_patch_parser_feedback_language_neutral text_artifact_short_serialized_markdown_rejected zero_diff_patch_rejected artifact_preservation".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "progress_effect=no_progress".to_string(),
                "common_file_change_admission".to_string(),
                "write_atomic_filechange_commit_transaction".to_string(),
                "write_no_content_fixture_language_neutral".to_string(),
                "apply_patch_operation_pre_admission".to_string(),
                "apply_patch_move_destination_fresh_authority".to_string(),
                "multi_path_edit_lock_set".to_string(),
                "apply_patch_single_invocation_permission_admission".to_string(),
                "apply_patch_permission_before_formatter_validation".to_string(),
                "apply_patch_tool_invocation_lock_scope".to_string(),
                "apply_patch_admitted_execution_plan".to_string(),
                "apply_patch_unique_participant_ownership".to_string(),
                "apply_patch_duplicate_participant_before_formatter_validation".to_string(),
                "apply_patch_atomic_commit_transaction".to_string(),
                "unambiguous_malformed_edit_argument_repair".to_string(),
                "malformed_edit_argument_repair_projection".to_string(),
                "empty_file_change_not_authoring_progress".to_string(),
                "corrective_content_shape_requires_typed_progress_class".to_string(),
                "invalid_edit_arguments_control_recovery_projection".to_string(),
                "loop_impl_invalid_edit_fixture_language_neutral".to_string(),
                "invalid_edit_recovery_candidate_target_operation".to_string(),
                "invalid_edit_recovery_uses_open_target_when_candidate_is_inactive".to_string(),
                "invalid_edit_recovery_candidate_target_normalized".to_string(),
                "mixed_target_apply_patch_active_hunk_evidence".to_string(),
                "mixed_target_invalid_edit_recovery_projection".to_string(),
                "invalid_edit_arguments_recovery_persists_across_final_message".to_string(),
                "invalid_apply_patch_required_action_surface_alignment".to_string(),
                "edit_recovery_fixture_workflow_neutral".to_string(),
                "loop_impl_invalid_edit_failed_edit_recovery_fixture_language_neutral".to_string(),
                "tool_orchestrator_fixture_language_neutral".to_string(),
                "parser_error".to_string(),
                "raw_argument_shape_hash".to_string(),
                "allowed_surface_snapshot".to_string(),
                "malformed_write_patch_capable_recovery_surface".to_string(),
                "loop_impl_malformed_write_fixture_language_neutral".to_string(),
                "malformed_apply_patch_patch_capable_recovery_surface".to_string(),
                "loop_impl_malformed_apply_patch_fixture_language_neutral".to_string(),
                "active_generated_test_malformed_apply_patch_required_action_surface_alignment"
                    .to_string(),
                "stale_inactive_malformed_apply_patch_stays_no_write_recovery".to_string(),
                "no_content_change".to_string(),
                "idempotent_file_write_no_progress".to_string(),
                "idempotent_apply_patch_no_progress".to_string(),
                "idempotent_file_write_terminal_guard".to_string(),
                "destructive_noop_acknowledgement_patch_rejected".to_string(),
                "empty_apply_patch_hunks_rejected".to_string(),
                "hunkless_update_patch_rejected".to_string(),
                "bare_markdown_update_body_rejected".to_string(),
                "add_file_unprefixed_content_line_feedback".to_string(),
                "edit_patch_parser_feedback_language_neutral".to_string(),
                "text_artifact_short_serialized_markdown_rejected".to_string(),
                "zero_diff_patch_rejected".to_string(),
                "artifact_preservation".to_string(),
            ],
            forbidden_refs: vec![
                "duplicate_success_cache".to_string(),
                "repair_progress_from_no_content_write".to_string(),
                "noop_acknowledgement_as_content_progress".to_string(),
                "zero_diff_patch_as_content_progress".to_string(),
                "raw_provider_path_spelling_as_recovery_target_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.active_authoring_rejects_wrong_target"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring active_deliverable_targets workspace_path_coordinate_authority escaped_windows_absolute_target_matches_relative_deliverable workspace_prefix_boundary ActiveWorkContract::Verification repair_required RepairOperationTemplate exact_target write_admission exact_write_wrong_path_content_shape_uses_active_target exact_apply_patch_wrong_path_content_shape_uses_active_target source_owned_repair_generated_test_rewrite_rejected loop_impl_repair_shell_exact_repair_write_fixture_language_neutral loop_impl_active_authoring_docs_regression_fixture_domain_neutral tool_orchestrator_fixture_language_neutral ToolLifecycleEnvelope FunctionCallOutput success=false wrong_authoring_target wrong_authoring_target_semantic_no_progress_key progress_effect=no_progress terminal_guard stable_tool_schema".to_string(),
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
                "exact_write_wrong_path_content_shape_uses_active_target".to_string(),
                "exact_apply_patch_wrong_path_content_shape_uses_active_target".to_string(),
                "source_owned_repair_generated_test_rewrite_rejected".to_string(),
                "loop_impl_repair_shell_exact_repair_write_fixture_language_neutral".to_string(),
                "loop_impl_active_authoring_docs_regression_fixture_domain_neutral".to_string(),
                "tool_orchestrator_fixture_language_neutral".to_string(),
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
            authority_source: "CodexHistoryItemStream ActiveWorkContract::Verification exact_shell_verification_authority runtime_owned_verification_command_dispatch repair_required_edit_surface repair_active_shell_probe_uses_repair_target_authority repair_lane_typed_target_projection_no_required_action_shim turn_decision_repair_target_exact_path_authority protocol_runtime_fixture_language_neutral loop_impl_repair_shell_exact_repair_write_fixture_language_neutral shell_command_satisfaction verification_command_encoding_alias FunctionCallOutput wrong_verification_command progress_effect=no_progress terminal_guard".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "ActiveWorkContract::Verification".to_string(),
                "exact_shell_verification_authority".to_string(),
                "runtime_owned_verification_command_dispatch".to_string(),
                "repair_required_edit_surface".to_string(),
                "repair_active_shell_probe_uses_repair_target_authority".to_string(),
                "repair_lane_typed_target_projection_no_required_action_shim".to_string(),
                "turn_decision_repair_target_exact_path_authority".to_string(),
                "protocol_runtime_fixture_language_neutral".to_string(),
                "loop_impl_repair_shell_exact_repair_write_fixture_language_neutral".to_string(),
                "shell_command_satisfaction".to_string(),
                "verification_command_encoding_alias".to_string(),
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
            authority_source: "CodexHistoryItemStream ActiveWorkContract::RequestedWorkAuthoring stable_tool_interface content_changing_satisfaction supporting_context progress_projection_saturation supporting_context_budget_recovery_surface authoring_supporting_context_target_grounding_read authoring_target_grounding_required loop_impl_operation_intent_fixture_language_neutral loop_impl_active_authoring_docs_regression_fixture_domain_neutral FunctionCallOutput progress_effect=no_progress terminal_guard".to_string(),
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
                "loop_impl_operation_intent_fixture_language_neutral".to_string(),
                "loop_impl_active_authoring_docs_regression_fixture_domain_neutral".to_string(),
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
            authority_source: "ActiveWorkContract::RequestedWorkAuthoring ActiveWorkContract::Verification progress_projection_call_output progress_projection_edit_recovery progress_projection_patch_only_surface progress_projection_target_grounding_read docs_content_grounding_progress_projection_preserves_grounding_surface authoring_supporting_context_budget_recovery_surface authoring_supporting_context_target_grounding_read grounding_target_matching_rejects_foreign_suffix_collision grounding_metadata_path_target_identity_exact docs_route_grep_line_path_generic_path_line multi_target_authoring_consumed_grounding_edit_recovery partial_target_grounding_remaining_target_authority singleton_missing_target_source_reference_or_create_authority stage_continuation_existing_target_grounding_read docs_existing_target_update_exact_read_grounding generated_test_consumed_source_reference_active_target_grounding repair_supporting_context_target_scoped_grounding verification_repair_supporting_context_loop_terminal_cluster repair_control_single_owner_projection source_repair_initial_target_grounding_survives_edit_narrowing source_repair_initial_grounding_precedes_edit_only_recovery failed_patch_context_mismatch_target_grounding patch_context_mismatch_recovery_augments_read_surface invalid_edit_recovery_exact_target_regrounding loop_impl_docs_budget_edit_surface_fixture_language_neutral no_required_action_string stable_tool_schema semantic_no_progress_guard".to_string(),
            required_refs: vec![
                "ActiveWorkContract::RequestedWorkAuthoring".to_string(),
                "ActiveWorkContract::Verification".to_string(),
                "progress_projection_call_output".to_string(),
                "progress_projection_edit_recovery".to_string(),
                "progress_projection_target_grounding_read".to_string(),
                "docs_content_grounding_progress_projection_preserves_grounding_surface"
                    .to_string(),
                "progress_projection_patch_only_surface".to_string(),
                "authoring_supporting_context_budget_recovery_surface".to_string(),
                "authoring_supporting_context_target_grounding_read".to_string(),
                "grounding_target_matching_rejects_foreign_suffix_collision".to_string(),
                "grounding_metadata_path_target_identity_exact".to_string(),
                "docs_route_grep_line_path_generic_path_line".to_string(),
                "multi_target_authoring_consumed_grounding_edit_recovery".to_string(),
                "partial_target_grounding_remaining_target_authority".to_string(),
                "singleton_missing_target_source_reference_or_create_authority".to_string(),
                "stage_continuation_existing_target_grounding_read".to_string(),
                "docs_existing_target_update_exact_read_grounding".to_string(),
                "generated_test_consumed_source_reference_active_target_grounding".to_string(),
                "repair_supporting_context_target_scoped_grounding".to_string(),
                "verification_repair_supporting_context_loop_terminal_cluster".to_string(),
                "repair_control_single_owner_projection".to_string(),
                "source_repair_initial_target_grounding_survives_edit_narrowing".to_string(),
                "source_repair_initial_grounding_precedes_edit_only_recovery".to_string(),
                "failed_patch_context_mismatch_target_grounding".to_string(),
                "patch_context_mismatch_recovery_augments_read_surface".to_string(),
                "invalid_edit_recovery_exact_target_regrounding".to_string(),
                "loop_impl_docs_budget_edit_surface_fixture_language_neutral".to_string(),
                "stable_tool_schema".to_string(),
                "semantic_no_progress_guard".to_string(),
            ],
            forbidden_refs: vec![
                "legacy_required_action_string_field".to_string(),
                "schema_const_target_authority".to_string(),
                "untyped_tool_surface_suppression".to_string(),
                "whole_file_write_progress_projection_surface".to_string(),
                "foreign_suffix_path_as_target_grounding_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.edit_surface_registry_symmetry"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolRouter core_edit_tool_surface_registry_symmetry apply_patch_fixture_language_neutral FunctionCallOutput wrong_authoring_target call_id_scoped_failed_inactive_write call_id_scoped_failed_inactive_apply_patch failed_inactive_executable_pair_omitted failed_inactive_non_executable_feedback_projected failed_inactive_argument_payload_omitted loop_impl_docs_budget_edit_surface_fixture_language_neutral write_visible apply_patch_visible successful_stale_inactive_payload_summary_only".to_string(),
            required_refs: vec![
                "ToolRouter".to_string(),
                "core_edit_tool_surface_registry_symmetry".to_string(),
                "apply_patch_fixture_language_neutral".to_string(),
                "FunctionCallOutput".to_string(),
                "wrong_authoring_target".to_string(),
                "call_id_scoped_failed_inactive_write".to_string(),
                "call_id_scoped_failed_inactive_apply_patch".to_string(),
                "failed_inactive_executable_pair_omitted".to_string(),
                "failed_inactive_non_executable_feedback_projected".to_string(),
                "failed_inactive_argument_payload_omitted".to_string(),
                "loop_impl_docs_budget_edit_surface_fixture_language_neutral".to_string(),
                "write_visible".to_string(),
                "apply_patch_visible".to_string(),
                "successful_stale_inactive_payload_summary_only".to_string(),
            ],
            forbidden_refs: vec![
                "anonymous_wrong_target_correction_only".to_string(),
                "hidden_core_tool".to_string(),
                "broad_write_saturation".to_string(),
                "failed_inactive_executable_pair_preserved".to_string(),
                "failed_inactive_raw_payload_replay".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.rejected_tool_semantic_terminal_guard"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "RejectedToolProposal semantic_no_progress_key invalid_edit_recovery_semantic_no_progress_key tool_allowed_false tool_choice_auto broad_surface_terminal_guard rejected_tool_required_action_terminal_guard lifecycle_kernel_provider_noncompliance provider_ignored_edit_only_surface malformed_tool_arguments_terminal_guard non_edit_invalid_tool_arguments_terminal_guard result_hash_evidence_not_key repair_required_edit_before_verification_rerun argument_payload_omitted projection_noise_absent".to_string(),
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
                "non_edit_invalid_tool_arguments_terminal_guard".to_string(),
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
            authority_source: "HistoryItem ActiveWorkContract::Verification repair_required=true TurnControlEnvelope ActionAuthority target_scoped_read_grounding repair_required_edit_surface turn_decision_repair_required_edit_surface_required repair_required_active_work_without_edit_surface".to_string(),
            required_refs: vec![
                "ActiveWorkContract".to_string(),
                "repair_required=true".to_string(),
                "ActionAuthority".to_string(),
                "target_scoped_read_grounding".to_string(),
                "repair_required_edit_surface".to_string(),
                "turn_decision_repair_required_edit_surface_required".to_string(),
                "repair_required_active_work_without_edit_surface".to_string(),
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
            authority_source: "ToolLifecycleEnvelope FunctionCallOutput success=false executed_tool_failure result_hash allowed_surface terminal_guard loop_impl_terminal_guard_fixture_language_neutral".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "FunctionCallOutput".to_string(),
                "success=false".to_string(),
                "executed_tool_failure".to_string(),
                "result_hash".to_string(),
                "terminal_guard".to_string(),
                "loop_impl_terminal_guard_fixture_language_neutral".to_string(),
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
            authority_source: "ToolLifecycleEnvelope synthetic_corrective_feedback no_verification_run_result generic_verification_runner_metadata_projection preserve_previous_VerificationFailureCluster repair_supporting_context_target_scoped_obligation truncated_tool_output_feedback_registered_tool_surface harness_no_progress_signature_schema_runtime_projection no_summary_parser_authority".to_string(),
            required_refs: vec![
                "ToolLifecycleEnvelope".to_string(),
                "synthetic_corrective_feedback".to_string(),
                "no_verification_run_result".to_string(),
                "generic_verification_runner_metadata_projection".to_string(),
                "preserve_previous_VerificationFailureCluster".to_string(),
                "repair_supporting_context_target_scoped_obligation".to_string(),
                "truncated_tool_output_feedback_registered_tool_surface".to_string(),
                "harness_no_progress_signature_schema_runtime_projection".to_string(),
            ],
            forbidden_refs: vec![
                "synthetic_feedback_as_verification_run".to_string(),
                "raw_summary_parser_authority".to_string(),
                "any_path_is_repair_context".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.workspace_relative_file_change_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "FileChangeEvidence workspace_relative_path workspace_path_separator_normalization workspace_prefix_boundary session_cwd_authority apply_patch_file_change_storage_uses_workspace_relative_paths no_route_root_relative_closeout_target GlobTool workspace_relative_pattern_match model_visible_relative_output search_glob_fixture_language_neutral edit_change_tracker_fixture_language_neutral".to_string(),
            required_refs: vec![
                "FileChangeEvidence".to_string(),
                "workspace_relative_path".to_string(),
                "workspace_path_separator_normalization".to_string(),
                "workspace_prefix_boundary".to_string(),
                "session_cwd_authority".to_string(),
                "apply_patch_file_change_storage_uses_workspace_relative_paths".to_string(),
                "GlobTool".to_string(),
                "workspace_relative_pattern_match".to_string(),
                "model_visible_relative_output".to_string(),
                "search_glob_fixture_language_neutral".to_string(),
                "edit_change_tracker_fixture_language_neutral".to_string(),
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
            authority_source: "ShellTool detected_file_changes absolute_paths EditSafety confirmed_content_baseline write_apply_patch_stale_guard baseline_snapshot_restore_on_filechange_persistence_failure".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "detected_file_changes".to_string(),
                "absolute_paths".to_string(),
                "EditSafety".to_string(),
                "confirmed_content_baseline".to_string(),
                "write_apply_patch_stale_guard".to_string(),
                "baseline_snapshot_restore_on_filechange_persistence_failure".to_string(),
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
            authority_source: "ShellTool stdout_bytes stderr_bytes display_projection shell_output_decode_strategy locale_fallback language_text_io_command_surface_adapter shell_output_text_encoding_contract".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "stdout_bytes".to_string(),
                "stderr_bytes".to_string(),
                "display_projection".to_string(),
                "shell_output_decode_strategy".to_string(),
                "locale_fallback".to_string(),
                "language_text_io_command_surface_adapter".to_string(),
                "shell_output_text_encoding_contract".to_string(),
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
            authority_source: "CommandTextEncodingReview command_text_encoding_contract language_text_io_command_surface_adapter text_io_surface encoding_explicit encoding_inherited_from_tool_environment encoding_unspecified powershell_get_content_utf8_explicit shell_command_text_encoding_fixture_language_neutral shell_contract_violation_typed_no_progress_feedback command_correction ToolResult corrective_result".to_string(),
            required_refs: vec![
                "command_text_encoding_contract".to_string(),
                "language_text_io_command_surface_adapter".to_string(),
                "encoding_explicit".to_string(),
                "encoding_inherited_from_tool_environment".to_string(),
                "encoding_unspecified".to_string(),
                "powershell_get_content_utf8_explicit".to_string(),
                "shell_command_text_encoding_fixture_language_neutral".to_string(),
                "shell_contract_violation_typed_no_progress_feedback".to_string(),
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
            authority_source: "ShellTool VerificationExecutionItem timeout cancellation normal_completion descendant_process_tree_first parent_shell_kill_after_tree manual_st_route_owned_verification_descendant_cleanup completion_descendant_cleanup_before_pipe_join bounded_wait no_orphan_child_process".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "timeout".to_string(),
                "cancellation".to_string(),
                "descendant_process_tree_first".to_string(),
                "parent_shell_kill_after_tree".to_string(),
                "manual_st_route_owned_verification_descendant_cleanup".to_string(),
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
            authority_source: "ShellTool external_connection_review environment_setup_review mandatory_user_confirmation shell_output_projection stdout stderr exit_code retry_guidance shell_syntax_correction_language_neutral".to_string(),
            required_refs: vec![
                "ShellTool".to_string(),
                "external_connection_review".to_string(),
                "environment_setup_review".to_string(),
                "shell_output_projection".to_string(),
                "stdout".to_string(),
                "stderr".to_string(),
                "exit_code".to_string(),
                "retry_guidance".to_string(),
                "shell_syntax_correction_language_neutral".to_string(),
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
            fixture_id: "fixture.tool_lifecycle.external_tool_surface_schema_validation"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "McpClient tools/list external_tool_surface_schema_validation typed_tool_descriptor_name fail_closed_malformed_descriptor model_visible_metadata mcp_tools_list_descriptor_schema_validation".to_string(),
            required_refs: vec![
                "McpClient".to_string(),
                "tools/list".to_string(),
                "external_tool_surface_schema_validation".to_string(),
                "typed_tool_descriptor_name".to_string(),
                "fail_closed_malformed_descriptor".to_string(),
                "model_visible_metadata".to_string(),
                "mcp_tools_list_descriptor_schema_validation".to_string(),
            ],
            forbidden_refs: vec![
                "silent_descriptor_drop".to_string(),
                "filter_map_tool_surface_authority".to_string(),
                "partial_external_tool_registry_success".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.tool_lifecycle.full_access_configured_boundary_authority"
                .to_string(),
            family: PreflightGateFamily::ToolLifecycleAuthority,
            authority_source: "ToolPermissionLifecycle AccessMode::FullAccess configured_workspace_boundary outside_workspace_review_required protected_workspace_authority_review_required network_review_required in_boundary_full_access_confirmation_free".to_string(),
            required_refs: vec![
                "ToolPermissionLifecycle".to_string(),
                "AccessMode::FullAccess".to_string(),
                "configured_workspace_boundary".to_string(),
                "outside_workspace_review_required".to_string(),
                "protected_workspace_authority_review_required".to_string(),
                "network_review_required".to_string(),
                "in_boundary_full_access_confirmation_free".to_string(),
            ],
            forbidden_refs: vec![
                "full_access_global_allow_all".to_string(),
                "outside_workspace_auto_allowed".to_string(),
                "protected_workspace_authority_auto_allowed".to_string(),
                "network_auto_allowed".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.vision.input_item_lifecycle_authority".to_string(),
            family: PreflightGateFamily::ProtocolItemLifecycle,
            authority_source: "CodexUserInput LocalImage ContentItem::InputImage image_label provider_visible_image_item diagnostic_source_path_not_workspace_authority consumed_vision_image_not_reattached_for_verification consumed_vision_image_not_reattached_for_verification_repair consumed_vision_image_not_reattached_in_chat_request".to_string(),
            required_refs: vec![
                "CodexUserInput".to_string(),
                "LocalImage".to_string(),
                "ContentItem::InputImage".to_string(),
                "image_label".to_string(),
                "provider_visible_image_item".to_string(),
                "consumed_vision_image_not_reattached_for_verification".to_string(),
                "consumed_vision_image_not_reattached_for_verification_repair".to_string(),
                "consumed_vision_image_not_reattached_in_chat_request".to_string(),
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
            authority_source: "CodexTurnContext absolute_cwd absolute_workspace_root fixed_harness_workspace_root non_empty_root shell_execution_context app_default_desktop_workspace_creation_error_propagation".to_string(),
            required_refs: vec![
                "CodexTurnContext".to_string(),
                "absolute_cwd".to_string(),
                "absolute_workspace_root".to_string(),
                "fixed_harness_workspace_root".to_string(),
                "shell_execution_context".to_string(),
                "app_default_desktop_workspace_creation_error_propagation".to_string(),
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
            authority_source: "TurnLifecycleKernel TurnLifecyclePlan LifecycleGuardState LifecycleGuardSnapshot lifecycle_guard_snapshot_hydration lifecycle_guard_snapshot_canonical_order loop_impl_lifecycle_guard_hydration_sequence_order stable_surface_default open_obligation_final_message_recovery failed_edit_final_message_recovery provider_noncompliance_edit_recovery provider_noncompliance_recovery_overrides_grounding wrong_target_authoring_edit_recovery wrong_target_code_authoring_patch_only_recovery wrong_target_generated_test_source_reference_read inactive_target_edit_recovery_reminder_uses_current_edit_surface malformed_apply_patch_recovery_overrides_stale_wrong_target provider_required_tool_choice_final_message_recovery docs_provider_required_final_message_required_tool_choice code_authoring_final_message_hard_edit_recovery tool_choice_auto hard_recovery_required replay_policy proposal_policy corrective_policy terminal_policy continuation_expectation diagnostics_projection".to_string(),
            required_refs: vec![
                "TurnLifecycleKernel".to_string(),
                "TurnLifecyclePlan".to_string(),
                "LifecycleGuardState".to_string(),
                "LifecycleGuardSnapshot".to_string(),
                "lifecycle_guard_snapshot_hydration".to_string(),
                "lifecycle_guard_snapshot_canonical_order".to_string(),
                "loop_impl_lifecycle_guard_hydration_sequence_order".to_string(),
                "stable_surface_default".to_string(),
                "open_obligation_final_message_recovery".to_string(),
                "failed_edit_final_message_recovery".to_string(),
                "provider_noncompliance_edit_recovery".to_string(),
                "provider_noncompliance_recovery_overrides_grounding".to_string(),
                "wrong_target_authoring_edit_recovery".to_string(),
                "wrong_target_code_authoring_patch_only_recovery".to_string(),
                "wrong_target_generated_test_source_reference_read".to_string(),
                "inactive_target_edit_recovery_reminder_uses_current_edit_surface".to_string(),
                "malformed_apply_patch_recovery_overrides_stale_wrong_target".to_string(),
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
            authority_source: "CodexResponsesRequest ActiveWorkContract RequestedWorkAuthoring candidate_tool_surface ActionAuthority workspace_target_identity_normalization stable_tool_schema tool_choice_auto provider_metadata_mode_tool_choice_serialization requested_work_singleton_stable_surface singleton_missing_target_apply_patch_action_auto_choice codex_style_code_authoring_omits_whole_file_write codex_style_code_authoring_omits_json_discovery_surface codex_style_docs_authoring_omits_non_codex_json_surface generated_test_source_reference_grounding_after_source_change generic_code_source_reference_grounding singleton_missing_target_source_reference_or_create_authority generated_test_consumed_source_reference_active_target_grounding loop_impl_generated_test_source_reference_fixture_domain_neutral repair_target_aliases_collapse_to_singleton_write_action loop_impl_singleton_write_argument_fixture_language_neutral typed_required_action_rendered_text explicit_required_action_conflict_fail_closed open_work_lifecycle_evidence normal_authoring_final_message_recovery_stable_surface failed_edit_final_message_recovery_keeps_failed_edit_surface docs_open_obligation_required_edit_recovery open_obligation_final_message_recovery_persists_across_no_progress_tool authoring_final_message_target_grounding_read docs_patch_context_final_message_grounding docs_existing_target_update_exact_read_grounding loop_impl_docs_existing_target_grounding_fixture_domain_neutral source_repair_exact_write_final_message_recovery hard_repair_recovery_executable_schema_surface harness_closeout_guard open_obligation_final_message_guard open_obligation_final_message_guard_context_key".to_string(),
            required_refs: vec![
                "CodexResponsesRequest".to_string(),
                "ActiveWorkContract".to_string(),
                "RequestedWorkAuthoring".to_string(),
                "ActionAuthority".to_string(),
                "workspace_target_identity_normalization".to_string(),
                "stable_tool_schema".to_string(),
                "tool_choice_auto".to_string(),
                "provider_metadata_mode_tool_choice_serialization".to_string(),
                "requested_work_singleton_stable_surface".to_string(),
                "singleton_missing_target_apply_patch_action_auto_choice".to_string(),
                "codex_style_code_authoring_omits_whole_file_write".to_string(),
                "codex_style_code_authoring_omits_json_discovery_surface".to_string(),
                "codex_style_docs_authoring_omits_non_codex_json_surface".to_string(),
                "generated_test_source_reference_grounding_after_source_change".to_string(),
                "generic_code_source_reference_grounding".to_string(),
                "singleton_missing_target_source_reference_or_create_authority".to_string(),
                "generated_test_consumed_source_reference_active_target_grounding".to_string(),
                "loop_impl_generated_test_source_reference_fixture_domain_neutral".to_string(),
                "repair_target_aliases_collapse_to_singleton_write_action".to_string(),
                "loop_impl_singleton_write_argument_fixture_language_neutral".to_string(),
                "typed_required_action_rendered_text".to_string(),
                "explicit_required_action_conflict_fail_closed".to_string(),
                "open_work_lifecycle_evidence".to_string(),
                "normal_authoring_final_message_recovery_stable_surface".to_string(),
                "failed_edit_final_message_recovery_keeps_failed_edit_surface".to_string(),
                "docs_open_obligation_required_edit_recovery".to_string(),
                "open_obligation_final_message_recovery_persists_across_no_progress_tool"
                    .to_string(),
                "authoring_final_message_target_grounding_read".to_string(),
                "docs_patch_context_final_message_grounding".to_string(),
                "docs_existing_target_update_exact_read_grounding".to_string(),
                "loop_impl_docs_existing_target_grounding_fixture_domain_neutral".to_string(),
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
            authority_source: "CodexHistoryItemStream VerificationFailureCluster ActiveWorkContract Verification repair_required_active_work SourceViolatesContract SourceTestContractMismatch TestViolatesContract source_owned_requirement_refs_align_active_work_with_repair_lane repair_lane_source_owned_fixture_language_neutral turn_decision_fixture_language_neutral turn_decision_repair_required_edit_surface_required repair_required_active_work_without_edit_surface contract_visible_public_exception_owner_authority generated_test_parse_defect_owner_authority generated_test_reflection_api_misuse_owner_authority generated_test_module_attribute_api_misuse_owner_authority generated_test_exception_type_overreach_owner_authority source_parse_defect_owner_authority no_tests_ran_recent_generated_test_target_authority generated_test_subprocess_encoding_owner_authority generated_test_subprocess_output_capture_owner_authority generated_test_name_resolution_owner_authority generated_test_import_nameerror_owner_authority mixed_source_test_contract_reconciliation_owner_authority generated_test_contract_overreach_owner_projection_alignment generic_generated_test_only_owner_target_authority ungrounded_generated_public_output_assertion_owner_authority generated_test_local_binding_contradiction_owner_authority deferred_verification_command_not_progress_evidence failed_patch_context_mismatch_target_grounding patch_context_mismatch_recovery_augments_read_surface ActionAuthority repair_lane_diagnostic_only stable_tool_schema call_id_scoped_outputs".to_string(),
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
                "repair_lane_source_owned_fixture_language_neutral".to_string(),
                "turn_decision_fixture_language_neutral".to_string(),
                "turn_decision_repair_required_edit_surface_required".to_string(),
                "repair_required_active_work_without_edit_surface".to_string(),
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
            authority_source: "CodexTurnComplete final_assistant_message no_synthetic_completion_tool no_closeout_reread_tool no_open_obligations answer_only_no_executable_work answer_only_final_message_lifecycle_fixture_language_neutral bounded_closeout_final_response_timeout closeout_timeout_no_synthetic_final_message satisfied_item_stream_terminal_guard".to_string(),
            required_refs: vec![
                "CodexTurnComplete".to_string(),
                "final_assistant_message".to_string(),
                "no_synthetic_completion_tool".to_string(),
                "no_open_obligations".to_string(),
                "answer_only_no_executable_work".to_string(),
                "answer_only_final_message_lifecycle_fixture_language_neutral".to_string(),
                "bounded_closeout_final_response_timeout".to_string(),
                "closeout_timeout_no_synthetic_final_message".to_string(),
                "satisfied_item_stream_terminal_guard".to_string(),
            ],
            forbidden_refs: vec![
                "synthetic_completion_required_action".to_string(),
                "mandatory_closeout_reread".to_string(),
                "unbounded_closeout_provider_wait".to_string(),
                "provider_timeout_synthetic_final_message".to_string(),
                "answer_only_final_message_rejected_by_closeout_flag".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.closeout.open_obligation_final_assistant_continuation_hook"
                .to_string(),
            family: PreflightGateFamily::ManualStEvidenceSchema,
            authority_source: "CodexTurnComplete StopRequest hook_prompt_message text_only_hook_prompt RuntimeCompleted RuntimeDidNotComplete final_assistant_message OpenObligation ManualStCloseoutEvidence CloseoutContinuationUserTurn missing_artifacts file_changing_tool_call_required expected_artifacts_inventory_non_authoring current_workspace_artifact_clears_stale_authoring_obligation satisfied_docs_repair_not_open_closeout route_verification_waits_for_artifact_authoring post_repair_route_verification_clears_stale_repair latest_verification_command_evidence current_run_error_closeout_projection runtime_error_open_obligation_continuation_budget runtime_terminal_status_open_obligation_continuation_budget same_workspace_continuation_budget terminalized_session_continuation_ledger terminal_cluster_signature typed_terminal_item_cluster tool_output_terminal_cluster_evidence tool_no_progress_terminal_cluster content_changing_authoring_no_progress_terminal_cluster content_changing_authoring_no_progress_route_fail_stop verification_non_convergence_terminal_cluster authoring_grounding_budget_terminal_cluster authoring_grounding_terminal_route_fail_stop unknown_terminal_reason_route_fail_stop ineffective_verification_repair_progress_does_not_reset_terminal_ledger stage_terminal_continuation_cap successful_continuation_case_verdict_materialization route_terminal_verdict_case_result_materialization open_obligation_final_message_surface_insensitive_guard bounded_route_failure".to_string(),
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
                "post_repair_route_verification_clears_stale_repair".to_string(),
                "latest_verification_command_evidence".to_string(),
                "current_run_error_closeout_projection".to_string(),
                "runtime_error_open_obligation_continuation_budget".to_string(),
                "runtime_terminal_status_open_obligation_continuation_budget".to_string(),
                "same_workspace_continuation_budget".to_string(),
                "terminalized_session_continuation_ledger".to_string(),
                "terminal_cluster_signature".to_string(),
                "typed_terminal_item_cluster".to_string(),
                "tool_output_terminal_cluster_evidence".to_string(),
                "tool_no_progress_terminal_cluster".to_string(),
                "content_changing_authoring_no_progress_terminal_cluster".to_string(),
                "content_changing_authoring_no_progress_route_fail_stop".to_string(),
                "verification_non_convergence_terminal_cluster".to_string(),
                "authoring_grounding_budget_terminal_cluster".to_string(),
                "authoring_grounding_terminal_route_fail_stop".to_string(),
                "unknown_terminal_reason_route_fail_stop".to_string(),
                "ineffective_verification_repair_progress_does_not_reset_terminal_ledger"
                    .to_string(),
                "stage_terminal_continuation_cap".to_string(),
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
            authority_source: "CodexTurnComplete StopRequest hook_prompt_message text_only_hook_prompt RuntimeCompleted final_assistant_message VerificationFailed latest_failed_command verification_failure_evidence repair_target write_or_apply_patch_required rerun_failed_command failure_signature_scoped_budget closeout_language_adapter_artifact_roles no_image_reattach".to_string(),
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
                "closeout_language_adapter_artifact_roles".to_string(),
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
            authority_source: "CodexHistoryItemStream FunctionCallOutput VerificationFailed evidence_labels language_failure_label_adapter diagnostic_traceback_paths continuation_context_sections requested_work_authoring_targets deliverable_artifact_paths ManualStCloseoutEvidence verification_repair_hook stable_tool_schema no_tool_closeout_prevention".to_string(),
            required_refs: vec![
                "CodexHistoryItemStream".to_string(),
                "FunctionCallOutput".to_string(),
                "VerificationFailed".to_string(),
                "evidence_labels".to_string(),
                "language_failure_label_adapter".to_string(),
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
            fixture_id: "fixture.design.thread_turn_item_protocol_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "ThreadTurnItemProtocol moyAI moyAI/src/protocol RequiredAction typed fields renderer output ActionAuthority dispatch surface ownership runtime capability hydration ToolOrchestrator-owned lifecycle snapshot preflight.control_envelope.dispatch_projection_authority historical implementation evidence boundary current authority map".to_string(),
            required_refs: vec![
                "ThreadTurnItemProtocol".to_string(),
                "moyAI".to_string(),
                "moyAI/src/protocol".to_string(),
                "RequiredAction typed fields".to_string(),
                "renderer output".to_string(),
                "ActionAuthority".to_string(),
                "dispatch surface ownership".to_string(),
                "runtime capability hydration".to_string(),
                "ToolOrchestrator-owned lifecycle snapshot".to_string(),
                "preflight.control_envelope.dispatch_projection_authority".to_string(),
                "historical implementation evidence boundary".to_string(),
                "current authority map".to_string(),
            ],
            forbidden_refs: vec![
                "moyai/src/protocol".to_string(),
                "action_string_dispatch_authority".to_string(),
                "`write:<target>` / `shell:<command>` などの executable action を示す場合"
                    .to_string(),
                "`TurnContext.allowed_tools` と `TurnContext.active_contract.allowed_tools` は同じ surface"
                    .to_string(),
                "`TurnContext.model_capabilities.supports_images=true`".to_string(),
                "R1 / R2 の Rust 実装は".to_string(),
                "unit test は同 module 内に置き".to_string(),
                "この順序を逆にしない".to_string(),
                "R1 skeleton".to_string(),
                "R4 で ToolRouter / ToolOrchestrator へ移行".to_string(),
                "control_envelope_projection_consistency".to_string(),
                "## 12. R2 以降の接続順".to_string(),
                "`TurnEngine` skeleton".to_string(),
                "ToolRouter / ToolOrchestrator migration".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.turn_decision_pipeline_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "TurnDecisionPipeline moyAI TurnControlEnvelope ActionAuthority ProjectionBundle typed lifecycle authority action-family authority adapter-owned evidence workflow-neutral invariant family provider dispatch ownership historical FR evidence boundary".to_string(),
            required_refs: vec![
                "TurnDecisionPipeline".to_string(),
                "moyAI".to_string(),
                "TurnControlEnvelope".to_string(),
                "ActionAuthority".to_string(),
                "ProjectionBundle".to_string(),
                "typed lifecycle authority".to_string(),
                "action-family authority".to_string(),
                "adapter-owned evidence".to_string(),
                "workflow-neutral invariant family".to_string(),
                "provider dispatch ownership".to_string(),
                "historical FR evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai` の manual ST failure".to_string(),
                "current `moyai` Turn Decision Pipeline".to_string(),
                "`moyai` provider dispatch".to_string(),
                "FR2-".to_string(),
                "FR03-".to_string(),
                "FR-".to_string(),
                "NO TESTS RAN".to_string(),
                "NameError".to_string(),
                "`write:<source>`".to_string(),
                "`write:<generated-test>`".to_string(),
                "`shell:<command>`".to_string(),
                "typed required action projection=write:".to_string(),
                "typed required action projection=shell:".to_string(),
                "python -m unittest".to_string(),
                "unittest".to_string(),
                "projectile movement".to_string(),
                "SCORE_PER_ROW".to_string(),
                "space_invader".to_string(),
                "calculator".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.root.spec_current_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "README.md ProjectBrief.md moyAI CodingAgent/moyAI moyAI/tests/manual_ST current root spec authority".to_string(),
            required_refs: vec![
                "README.md".to_string(),
                "ProjectBrief.md".to_string(),
                "moyAI".to_string(),
                "CodingAgent/moyAI".to_string(),
                "moyAI/tests/manual_ST".to_string(),
                "current root spec authority".to_string(),
            ],
            forbidden_refs: vec![
                "moyai root spec authority".to_string(),
                "CodingAgent/moyai".to_string(),
                "moyai/tests/manual_ST".to_string(),
                "`moyai/` 以下の Rust コード".to_string(),
                "current `moyai`".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.runtime_contracts_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "RuntimeContracts moyAI current build runtime contract current-state contract authority normative current-body product authority Codex primary comparison Roo Code local-LLM supplemental comparison moyAI/src/agent/state.rs moyAI/src/agent/prompt.rs moyAI/src/agent/loop_impl.rs moyAI/src/agent/lifecycle_kernel.rs moyAI/src/agent/tool_orchestrator.rs moyAI/src/agent/verification.rs moyAI/src/agent/tool_result_classification.rs moyAI/src/tool/todo_write.rs".to_string(),
            required_refs: vec![
                "RuntimeContracts".to_string(),
                "moyAI".to_string(),
                "current build".to_string(),
                "runtime contract".to_string(),
                "current-state contract authority".to_string(),
                "normative current-body product authority".to_string(),
                "Codex primary comparison".to_string(),
                "Roo Code local-LLM supplemental comparison".to_string(),
                "moyAI/src/agent/state.rs".to_string(),
                "moyAI/src/agent/prompt.rs".to_string(),
                "moyAI/src/agent/loop_impl.rs".to_string(),
                "moyAI/src/agent/lifecycle_kernel.rs".to_string(),
                "moyAI/src/agent/tool_orchestrator.rs".to_string(),
                "moyAI/src/agent/verification.rs".to_string(),
                "moyAI/src/agent/tool_result_classification.rs".to_string(),
                "moyAI/src/tool/todo_write.rs".to_string(),
            ],
            forbidden_refs: vec![
                "current build の `moyai`".to_string(),
                "moyai/src/".to_string(),
                "`moyai` では".to_string(),
                "`moyai` 側".to_string(),
                "OpenClaw".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.verification_harness_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "VerificationHarness moyAI current build verification harness authority moyAI/src/tui/app.rs moyAI/src/desktop/app.rs moyAI/src/desktop/tauri_app.rs moyAI/src/desktop/web_model.rs moyAI/tests/*.rs moyAI/tests/support/harness.rs moyAI/tests/manual_ST/README.md http://127.0.0.1:1234 qwen/qwen3.6-35b-a3b lm_studio_native_required 131072 context window 8192 max output tokens".to_string(),
            required_refs: vec![
                "VerificationHarness".to_string(),
                "moyAI".to_string(),
                "current build".to_string(),
                "verification harness authority".to_string(),
                "moyAI/src/tui/app.rs".to_string(),
                "moyAI/src/desktop/app.rs".to_string(),
                "moyAI/src/desktop/tauri_app.rs".to_string(),
                "moyAI/src/desktop/web_model.rs".to_string(),
                "moyAI/tests/*.rs".to_string(),
                "moyAI/tests/support/harness.rs".to_string(),
                "moyAI/tests/manual_ST/README.md".to_string(),
                "http://127.0.0.1:1234".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "lm_studio_native_required".to_string(),
                "131072 context window".to_string(),
                "8192 max output tokens".to_string(),
            ],
            forbidden_refs: vec![
                "current build の `moyai`".to_string(),
                "moyai/src/".to_string(),
                "moyai/tests".to_string(),
                "http://192.168.10.103:1234".to_string(),
                "openai-compatible-fixture-model".to_string(),
                "python -X utf8 widget.py 8 +".to_string(),
                "generic `test_widget.py`".to_string(),
                "exact `test_widget.py` repair".to_string(),
                "exact `widget.py` repair".to_string(),
                "generic `widget.py` / `test_widget.py`".to_string(),
                "generic `test_widget.py` / `test_other.py`".to_string(),
                "generic `docs/widget-design.md` target".to_string(),
                "generic `tool.py` commands".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.item_lifecycle_detail_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "ItemLifecycleDetailDesign moyAI current single control-plane item lifecycle Thread / Turn / Item protocol TurnControlEnvelope ActionAuthority ProjectionBundle ToolLifecycleEnvelope runtime capability hydration event-sourced runtime route-owned obligations app boundary projection memory / compaction continuity active preflight gate families historical FR evidence boundary".to_string(),
            required_refs: vec![
                "ItemLifecycleDetailDesign".to_string(),
                "moyAI".to_string(),
                "current single control-plane item lifecycle".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "TurnControlEnvelope".to_string(),
                "ActionAuthority".to_string(),
                "ProjectionBundle".to_string(),
                "ToolLifecycleEnvelope".to_string(),
                "runtime capability hydration".to_string(),
                "event-sourced runtime".to_string(),
                "route-owned obligations".to_string(),
                "app boundary projection".to_string(),
                "memory / compaction continuity".to_string(),
                "active preflight gate families".to_string(),
                "historical FR evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "moyai/".to_string(),
                "`moyai` runtime / protocol / harness target design".to_string(),
                "widget.py / test_widget.py".to_string(),
                "component.py / test_component.py".to_string(),
                "`moyai` current item lifecycle authority".to_string(),
                "`moyai` design consequence".to_string(),
                "moyai Current Item Lifecycle".to_string(),
                "moyai/src/".to_string(),
                "docs/component-design.md".to_string(),
                "As of the 2026-05-26 Codex alignment implementation pass".to_string(),
                "TurnLifecyclePlan owns dispatch".to_string(),
                "TurnLifecycleKernel owns provider edit surface narrowing".to_string(),
                "`TurnLifecyclePlan`, `ActionAuthority`, `ProjectionBundle`, and `TurnControlEnvelope`"
                    .to_string(),
                "2026-05-25 Codex-Aligned Lifecycle Kernel Redesign".to_string(),
                "`TurnLifecycleKernel` | Own the turn-level decision graph".to_string(),
                "the lifecycle kernel records it as provider".to_string(),
                "lifecycle kernel / provider\r\nadapter boundary".to_string(),
                "lifecycle kernel / provider\nadapter boundary".to_string(),
                "source artifact target must receive executable source payload".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.tiered_quality_gates_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "TieredQualityGates route taxonomy route-owned artifact evidence invariant / artifact-role authority typed lifecycle evidence adapter-owned verification evidence user-overridden model gate and fresh rerun boundary gate-family ordering".to_string(),
            required_refs: vec![
                "TieredQualityGates".to_string(),
                "route taxonomy".to_string(),
                "route-owned artifact evidence".to_string(),
                "invariant / artifact-role authority".to_string(),
                "typed lifecycle evidence".to_string(),
                "adapter-owned verification evidence".to_string(),
                "user-overridden model gate and fresh rerun boundary".to_string(),
                "gate-family ordering".to_string(),
            ],
            forbidden_refs: vec![
                "fresh representative behavior stopper".to_string(),
                "representative route starts from case1".to_string(),
                "all required cases pass".to_string(),
                "legacy case artifact maps".to_string(),
                "case artifact maps to expected gate results".to_string(),
                "exact verification rerun".to_string(),
                "exact verification rerun まで到達".to_string(),
                "provider request tools".to_string(),
                "tool_choice=required".to_string(),
                "verification lane で non-equivalent shell".to_string(),
                "representative Desktop GUI route".to_string(),
                "Desktop GUI e2e result".to_string(),
                "model availability gate result".to_string(),
                "configured model and required metadata available".to_string(),
                "manual ST の特定 case".to_string(),
                "shared `QualityGateResult` model and enums".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.current_authority_index_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CurrentAuthorityIndex moyAI SingleControlPlane ToolLifecycleOwner ProjectionSeparation HarnessEvidence".to_string(),
            required_refs: vec![
                "CurrentAuthorityIndex".to_string(),
                "moyAI".to_string(),
                "SingleControlPlane".to_string(),
                "ToolLifecycleOwner".to_string(),
                "ProjectionSeparation".to_string(),
                "HarnessEvidence".to_string(),
            ],
            forbidden_refs: vec!["moyai".to_string()],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.codex_lifecycle_conformance_audit_authority"
                .to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "CodexLifecycleConformanceAudit moyAI canonical item stream Thread / Turn / Item protocol TurnControlEnvelope ActionAuthority dispatch surface ownership runtime capability hydration ToolLifecycleEnvelope projection separation route obligations model-visible design-visible active preflight gate families historical incident evidence boundary".to_string(),
            required_refs: vec![
                "CodexLifecycleConformanceAudit".to_string(),
                "moyAI".to_string(),
                "canonical item stream".to_string(),
                "Thread / Turn / Item protocol".to_string(),
                "TurnControlEnvelope".to_string(),
                "ActionAuthority dispatch surface ownership".to_string(),
                "runtime capability hydration".to_string(),
                "ToolLifecycleEnvelope".to_string(),
                "projection separation".to_string(),
                "route obligations model-visible design-visible".to_string(),
                "active preflight gate families".to_string(),
                "historical incident evidence boundary".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Phase12 lifecycle hardening".to_string(),
                "exact verification rerun".to_string(),
                "concrete required write dispatch authority".to_string(),
                "python -m unittest".to_string(),
                "FR10-2026-05-21-018 Classification".to_string(),
                "current pre-fix checkpoint".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: true,
        },
        PreflightFixture {
            fixture_id: "fixture.design.replay_first_harness_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "ReplayFirstHarness moyAI route taxonomy route-owned replay evidence invariant replay fixture".to_string(),
            required_refs: vec![
                "ReplayFirstHarness".to_string(),
                "moyAI".to_string(),
                "route taxonomy".to_string(),
                "route-owned replay evidence".to_string(),
                "invariant replay fixture".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "future `moyai/` schema modules".to_string(),
                "manual-st-20260421-case2".to_string(),
                "manual_ST.case2".to_string(),
                "Desktop GUI e2e を case1 から再開".to_string(),
                "Case2 supplies a current representative fixture".to_string(),
                "latest known stopper".to_string(),
                "Expected classification for the latest known stopper".to_string(),
                "exact unittest rerun".to_string(),
                "collision behavior lacks shared public contract".to_string(),
                "pre-policy case2 evidence".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.design.run_store_event_log_authority".to_string(),
            family: PreflightGateFamily::DesignAuthority,
            authority_source: "RunStoreEventLogDesign moyAI Agent Harness Engine typed feedback envelope typed required action projection route-owned verification command contract route taxonomy E2E gate".to_string(),
            required_refs: vec![
                "RunStoreEventLogDesign".to_string(),
                "moyAI".to_string(),
                "Agent Harness Engine".to_string(),
                "typed feedback envelope".to_string(),
                "typed required action projection".to_string(),
                "route-owned verification command contract".to_string(),
                "route taxonomy E2E gate".to_string(),
            ],
            forbidden_refs: vec![
                "`moyai`".to_string(),
                "Required action、result hash、model-visible correction".to_string(),
                "representative scenario の behavior-level blocker".to_string(),
                "exact `python -m unittest` rerun lane".to_string(),
                "final Desktop GUI route proof".to_string(),
                "FR-099".to_string(),
                "unavailable `read`".to_string(),
                "route-owned behavior blocker".to_string(),
                "verification rerun lane".to_string(),
                "failing unittest stdout / stderr".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.verification.typed_evidence_cluster_authority".to_string(),
            family: PreflightGateFamily::VerificationEvidenceAuthority,
            authority_source: "VerificationRunResult VerificationFailureCluster VerificationFailureEvidence requirement_refs artifact_refs evidence_markers verification_repair_cycle_canonical_history_order verification_history_sequence_primary_order verification_repair_cycle_history_item_authority".to_string(),
            required_refs: vec![
                "VerificationFailureCluster".to_string(),
                "VerificationFailureEvidence".to_string(),
                "requirement_refs".to_string(),
                "verification_repair_cycle_canonical_history_order".to_string(),
                "verification_history_sequence_primary_order".to_string(),
                "verification_repair_cycle_history_item_authority".to_string(),
            ],
            forbidden_refs: vec![
                "failure_summary_contains_authority".to_string(),
                "raw_vector_order_as_verification_repair_authority".to_string(),
                "timestamp_primary_verification_history_order".to_string(),
                "transcript_repair_cycle_authority".to_string(),
            ],
            required_artifacts: Vec::new(),
            fail_closed_on_missing_typed_projection: false,
        },
        PreflightFixture {
            fixture_id: "fixture.desktop_transcript.completed_primary_reading_path".to_string(),
            family: PreflightGateFamily::DesktopTranscriptProjectionAuthority,
            authority_source: "DesktopTranscriptProjection canonical_turn_item_stream chronological_turn_blocks canonical_transcript_sequence_order primary_reading_path work_summary_completed collapsed_work_history final_assistant_closeout desktop_pseudo_tool_call_closeout_evidence_preserved desktop_query_fixture_current_provider_profile desktop_query_todo_status_typed_projection desktop_gui_typed_visibility_projection desktop_web_access_mode_typed_projection desktop_state_fixture_current_provider_profile desktop_session_row_status_typed_projection desktop_transcript_row_kind_typed_projection desktop_preferences_atomic_commit app_initial_turn_route_key_projection cli_renderer_fixture_current_provider_profile cli_human_renderer_typed_lifecycle_projection session_service_fixture_current_provider_profile session_transcript_fixture_current_provider_profile storage_repository_fixture_current_provider_profile desktop_open_transcript_markdown_evidence_preserved desktop_markdown_export_atomic_commit session_markdown_blocked_action_evidence_preserved session_markdown_legacy_toolcall_display_arguments_not_typed_projection desktop_file_change_runtime_path_evidence_preserved desktop_file_change_action_typed_projection desktop_transcript_fixture_language_neutral desktop_query_projection_fixture_language_neutral_runtime_evidence_preserved app_session_title_fixture_domain_neutral cli_json_history_renderer_respects_reasoning_visibility typed_terminal_outcome_authority typed_file_change_rows call_id_scoped_file_change_rows typed_transcript_row_kind typed_gui_visibility intermediate_assistant_folded control_feedback_folded tool_feedback_folded".to_string(),
            required_refs: vec![
                "DesktopTranscriptProjection".to_string(),
                "canonical_turn_item_stream".to_string(),
                "chronological_turn_blocks".to_string(),
                "canonical_transcript_sequence_order".to_string(),
                "primary_reading_path".to_string(),
                "work_summary_completed".to_string(),
                "collapsed_work_history".to_string(),
                "final_assistant_closeout".to_string(),
                "desktop_pseudo_tool_call_closeout_evidence_preserved".to_string(),
                "desktop_query_fixture_current_provider_profile".to_string(),
                "desktop_query_todo_status_typed_projection".to_string(),
                "desktop_gui_typed_visibility_projection".to_string(),
                "desktop_web_access_mode_typed_projection".to_string(),
                "desktop_state_fixture_current_provider_profile".to_string(),
                "desktop_session_row_status_typed_projection".to_string(),
                "desktop_transcript_row_kind_typed_projection".to_string(),
                "desktop_preferences_atomic_commit".to_string(),
                "app_initial_turn_route_key_projection".to_string(),
                "cli_renderer_fixture_current_provider_profile".to_string(),
                "cli_human_renderer_typed_lifecycle_projection".to_string(),
                "session_service_fixture_current_provider_profile".to_string(),
                "session_transcript_fixture_current_provider_profile".to_string(),
                "storage_repository_fixture_current_provider_profile".to_string(),
                "desktop_open_transcript_markdown_evidence_preserved".to_string(),
                "desktop_markdown_export_atomic_commit".to_string(),
                "session_markdown_blocked_action_evidence_preserved".to_string(),
                "session_markdown_legacy_toolcall_display_arguments_not_typed_projection"
                    .to_string(),
                "desktop_file_change_runtime_path_evidence_preserved".to_string(),
                "desktop_file_change_action_typed_projection".to_string(),
                "desktop_transcript_fixture_language_neutral".to_string(),
                "desktop_query_projection_fixture_language_neutral_runtime_evidence_preserved"
                    .to_string(),
                "app_session_title_fixture_domain_neutral".to_string(),
                "cli_json_history_renderer_respects_reasoning_visibility".to_string(),
                "typed_terminal_outcome_authority".to_string(),
                "typed_file_change_rows".to_string(),
                "call_id_scoped_file_change_rows".to_string(),
                "typed_transcript_row_kind".to_string(),
                "typed_gui_visibility".to_string(),
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
            authority_source: "route_manifest case_progress verification_command_log workspace_diff_manifest request_payload_summary timeout_classification active_case_progress_status inflight_case_session_progress route_inflight_timeout_classification_owner case_progress_phase_boundaries route_workspace_diff_language_adapter_content_roles prompt_visible_scenario_contract_authority stage_scoped_verification_spec_authority manual_st_expected_artifacts_spec_owned explicit_session_continuation no_continue_last_with_session provider_config_inheritance provider_stream_idle_timeout_classification provider_stream_retry_exhausted_classification provider_stream_retry_exhausted_timeout_owner provider_transport_stream_error_classification semantic_no_progress_terminal_classification route_owned_command_timeout route_command_stdin_closed event_stream_identity_coherence manual_st_closeout_exact_target_identity manual_st_generic_verification_command_contract manual_st_route_preflight_report_codex_style_admission manual_st_closeout_route_fixture_workflow_neutral_current_profile manual_st_reference_export_scope_hygiene testing_metadata_current_guard_index stored_artifact_classifier_fixture_language_neutral harness_replay_report_latest_run_lifecycle_order artifact_replay_route_evidence_content_schema".to_string(),
            required_refs: vec![
                "route_manifest".to_string(),
                "case_progress".to_string(),
                "verification_command_log".to_string(),
                "workspace_diff_manifest".to_string(),
                "active_case_progress_status".to_string(),
                "inflight_case_session_progress".to_string(),
                "route_inflight_timeout_classification_owner".to_string(),
                "case_progress_phase_boundaries".to_string(),
                "route_workspace_diff_language_adapter_content_roles".to_string(),
                "prompt_visible_scenario_contract_authority".to_string(),
                "stage_scoped_verification_spec_authority".to_string(),
                "manual_st_expected_artifacts_spec_owned".to_string(),
                "explicit_session_continuation".to_string(),
                "no_continue_last_with_session".to_string(),
                "provider_config_inheritance".to_string(),
                "provider_stream_idle_timeout_classification".to_string(),
                "provider_stream_retry_exhausted_classification".to_string(),
                "provider_stream_retry_exhausted_timeout_owner".to_string(),
                "provider_transport_stream_error_classification".to_string(),
                "semantic_no_progress_terminal_classification".to_string(),
                "route_owned_command_timeout".to_string(),
                "event_stream_identity_coherence".to_string(),
                "manual_st_closeout_exact_target_identity".to_string(),
                "manual_st_generic_verification_command_contract".to_string(),
                "manual_st_route_preflight_report_codex_style_admission".to_string(),
                "manual_st_closeout_route_fixture_workflow_neutral_current_profile".to_string(),
                "manual_st_reference_export_scope_hygiene".to_string(),
                "testing_metadata_current_guard_index".to_string(),
                "stored_artifact_classifier_fixture_language_neutral".to_string(),
                "harness_replay_report_latest_run_lifecycle_order".to_string(),
                "artifact_replay_route_evidence_content_schema".to_string(),
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

pub fn protocol_store_latest_turn_position_resists_timestamp_drift_fixture_passes() -> bool {
    crate::protocol::protocol_store_latest_turn_position_resists_timestamp_drift_fixture_passes()
}

pub fn manual_st_reference_exports_scope_hygiene_fixture_passes() -> bool {
    let manual_st_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/manual_ST");
    let mut files = Vec::new();
    collect_manual_st_reference_files(&manual_st_dir, &mut files);

    files.into_iter().all(|file| {
        let Ok(content) = fs::read_to_string(&file) else {
            return false;
        };
        !content.lines().any(|line| {
            BANNED_SCOPE_SHRINK_TERMS
                .iter()
                .any(|term| line.contains(term))
        })
    })
}

pub fn testing_metadata_current_guard_index_fixture_passes() -> bool {
    let metadata_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("docs").join("testing").join("test-metadata.json"));
    let Some(metadata_path) = metadata_path else {
        return false;
    };
    let Ok(metadata_text) = fs::read_to_string(metadata_path) else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<Value>(&metadata_text) else {
        return false;
    };
    let Some(tests) = metadata.get("tests").and_then(Value::as_array) else {
        return false;
    };
    let entries = tests
        .iter()
        .filter_map(|entry| {
            entry
                .get("test_id")
                .and_then(Value::as_str)
                .map(|id| (id, entry))
        })
        .collect::<BTreeMap<_, _>>();

    [
        "codex_style_preflight::preflight_artifact_rejects_empty_route_evidence",
        "manual_st_spec_authority::manual_st_reference_exports_do_not_preserve_scope_shrink_wording",
        "testing_metadata_authority::test_metadata_includes_current_preflight_and_manual_st_guards",
        "docs_testing_authority::preflight_gate_suite_docs_do_not_name_component_widget_as_generic_active_fixture_authority",
        "docs_testing_authority::preflight_gate_suite_docs_do_not_allow_component_arcade_fixture_payload_authority",
        "docs_testing_authority::preflight_gate_suite_docs_do_not_allow_widget_generated_test_payload_authority",
        "docs_testing_authority::preflight_gate_suite_docs_marker_projects_full_workflow_neutral_scope",
        "docs_testing_authority::testing_small_docs_use_current_product_authority",
        "docs_testing_authority::flow_contract_harness_map_uses_current_path_comparison_and_route_neutral_authority",
        "docs_testing_authority::agent_harness_architecture_uses_typed_action_and_route_verification_authority",
        "docs_testing_authority::agent_harness_components_uses_current_product_authority",
        "docs_testing_authority::agent_state_machine_uses_typed_lifecycle_authority",
        "docs_testing_authority::agent_harness_implementation_design_uses_current_authority",
        "docs_testing_authority::typed_contract_inventory_uses_current_provider_and_path_authority",
        "docs_testing_authority::thread_turn_item_protocol_uses_current_path_and_typed_required_action_authority",
        "docs_testing_authority::turn_decision_pipeline_uses_current_product_authority",
        "docs_testing_authority::runtime_contracts_use_current_product_authority",
        "docs_testing_authority::item_lifecycle_detail_design_uses_current_product_authority",
        "docs_testing_authority::tiered_quality_gates_use_route_taxonomy_and_invariant_authority",
        "docs_testing_authority::current_authority_index_uses_current_product_authority",
        "docs_testing_authority::failure_registry_header_projects_current_required_entry_schema",
        "docs_testing_authority::failure_registry_markdown_json_status_parity",
        "docs_testing_authority::failure_registry_pending_status_cannot_claim_verified_regression_evidence",
        "docs_testing_authority::failure_registry_verified_status_cannot_claim_pending_regression_plan",
        "docs_testing_authority::failure_registry_verified_status_cannot_claim_future_action_regression_plan",
        "docs_testing_authority::failure_registry_regression_fixture_authority_is_workflow_neutral",
        "docs_testing_authority::failure_registry_rerun_exposed_status_projects_verified_lifecycle",
        "docs_testing_authority::failure_registry_verified_pending_status_cannot_outlive_blocker_resolution",
        "docs_testing_authority::failure_registry_verified_status_cannot_retain_pending_investigation_root_cause",
    ]
    .into_iter()
    .all(|test_id| {
        entries.get(test_id).is_some_and(|entry| {
            entry.get("llm_mode").and_then(Value::as_str) == Some("no_llm")
                && entry.get("uses_live_llm").and_then(Value::as_bool) == Some(false)
                && entry.get("deterministic").and_then(Value::as_bool) == Some(true)
                && entry.get("promotion_status").and_then(Value::as_str) == Some("active")
        })
    })
}

pub fn preflight_gate_suite_docs_component_widget_fixture_authority_absent_fixture_passes() -> bool
{
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("testing")
                .join("PreflightGateSuite.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };
    content.contains("The active preflight implementation is `moyAI/src/harness/preflight.rs`.")
        && !content
            .contains("The active preflight implementation is `moyai/src/harness/preflight.rs`.")
        && ![
            "active fixture uses generic `test_widget.py`",
            "active fixture uses generic `component.py`",
            "a moyai shell pass",
            "generic `tool.py`",
            "generic `test_tool.py`",
            "`python tool.py",
            "`node tool.js",
            "`tool.test.js`",
        ]
        .into_iter()
        .any(|stale_authority| content.contains(stale_authority))
}

pub fn preflight_gate_suite_docs_component_arcade_fixture_payload_authority_absent_fixture_passes()
-> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("testing")
                .join("PreflightGateSuite.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };
    ![
        "`component.py` is already grounded",
        "`test_component.py` is not",
        "`read(component.py)`",
        "`component.py` / `test_component.py`",
        "`arcade_game.py` / `test_arcade_game.py`",
        "are allowed as fixture payloads",
    ]
    .into_iter()
    .any(|stale_authority| content.contains(stale_authority))
}

pub fn preflight_gate_suite_docs_widget_generated_test_payload_authority_absent_fixture_passes()
-> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("testing")
                .join("PreflightGateSuite.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };
    !content.contains("`test_widget.py`")
}

pub fn preflight_gate_suite_docs_marker_full_workflow_neutral_scope_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("testing")
                .join("PreflightGateSuite.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };
    content.contains(
        "active-fixture prose, generic fixture payload prose, and generated-test grounding examples",
    ) && content.contains(
        "without presenting component/widget/domain filenames or widget-domain generated-test targets as generic fixture authority",
    )
}

pub fn testing_small_docs_current_product_authority_fixture_passes() -> bool {
    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let docs = [
        root.join("docs").join("testing").join("TestingStrategy.md"),
        root.join("docs")
            .join("testing")
            .join("ManualSTRouteTaxonomy.md"),
        root.join("docs")
            .join("testing")
            .join("ManualSTEvidenceArtifacts.md"),
        root.join("docs").join("testing").join("CIReport.md"),
        root.join("docs")
            .join("testing")
            .join("QuarantinePolicy.md"),
    ];
    let mut combined = String::new();
    for doc in docs {
        let Ok(content) = fs::read_to_string(doc) else {
            return false;
        };
        combined.push_str(&content);
        combined.push('\n');
    }

    combined.contains("current `moyAI` design")
        && combined.contains("previous `moyAI` behavior")
        && combined.contains("legacy `moyAI` behavior")
        && combined.contains("the `moyAI` divergence")
        && ![
            "current `moyai` design",
            "previous `moyai` behavior",
            "legacy `moyai` behavior",
            "old `moyai` behavior",
            "the `moyai` divergence",
            "current moyai design",
            "legacy moyai behavior",
        ]
        .into_iter()
        .any(|stale_authority| combined.contains(stale_authority))
}

pub fn basic_design_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("docs").join("design").join("basic-design.md"));
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Basic Design")
        && content.contains("BasicDesign")
        && content.contains("`moyAI`")
        && content.contains("`moyAI/`")
        && content.contains("Codex-style typed lifecycle")
        && content.contains("single control-plane")
        && content.contains("event-sourced runtime")
        && content.contains("Desktop/App architecture")
        && content.contains("Agent Harness Engine")
        && content.contains("route-owned verification evidence")
        && content.contains("current closed-network provider boundary")
        && content.contains("historical phase evidence boundary");
    let stale_authority = [
        "`moyai`",
        "moyai/",
        "moyai/src",
        "Phase3 基本設計",
        "Phase5 の入出力は CLI を先行実装",
        "CLI 先行・単体バイナリ",
        "local LM Studio",
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "list / glob / grep / read",
        "`apply_patch` と whole-file `write`",
        "exact crate selection は Phase4",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn feature_inventory_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("feature-inventory.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Feature Inventory")
        && content.contains("FeatureInventory")
        && content.contains("`moyAI`")
        && content.contains("capability taxonomy")
        && content.contains("Codex-style typed lifecycle")
        && content.contains("typed action-family authority")
        && content.contains("adapter-owned evidence")
        && content.contains("route-owned verification evidence")
        && content.contains("Desktop/App architecture")
        && content.contains("Agent Harness Engine")
        && content.contains("current closed-network provider boundary")
        && content.contains("historical reference evidence boundary")
        && content.contains("non-adopted scope boundary");
    let stale_authority = [
        "`moyai`",
        "moyai/",
        "Phase3",
        "Phase5",
        "CLI adapter を先行",
        "list / glob / grep / read",
        "`apply_patch`",
        "whole-file `write`",
        "LM Studio `/api/v1/models`",
        "qwen/qwen3.6-35b-a3b",
        "採否ラベル",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn desktop_app_basic_design_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("desktop-app-basic-design.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Desktop App Basic Design")
        && content.contains("DesktopAppBasicDesign")
        && content.contains("`moyAI`")
        && content.contains("Desktop/App architecture")
        && content.contains("typed adapter ownership")
        && content.contains("canonical item projection")
        && content.contains("file-change evidence")
        && content.contains("Markdown export evidence")
        && content.contains("current closed-network provider boundary")
        && content.contains("historical implementation evidence boundary");
    let stale_authority = [
        "`moyai`",
        "moyai-desktop",
        "moyai desktop",
        "npm run build:desktop-web",
        "cargo build --release --bin",
        "127.0.0.1",
        "旧 Slint",
        "left navigation rail",
        "right artifact / preview pane",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn desktop_app_detailed_design_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("desktop-app-detailed-design.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Desktop App Detailed Design")
        && content.contains("DesktopAppDetailedDesign")
        && content.contains("`moyAI`")
        && content.contains("Desktop/App typed adapter contracts")
        && content.contains("canonical item projection")
        && content.contains("file-change evidence")
        && content.contains("Markdown export evidence")
        && content.contains("permission projection boundary")
        && content.contains("provider projection boundary")
        && content.contains("config projection boundary")
        && content.contains("route-owned verification evidence")
        && content.contains("current closed-network provider boundary")
        && content.contains("historical implementation evidence boundary");
    let stale_authority = [
        "`moyai`",
        "moyai/",
        "moyai desktop",
        "moyai-desktop",
        "LM Studio",
        "vLLM-MLX",
        "`/api/v1/models`",
        "`/health`",
        "npm run",
        "cargo build",
        "cargo test",
        "Implementation Order",
        "left navigation rail",
        "right artifact / preview pane",
        "representative route",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn tui_design_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("docs").join("design").join("tui-design.md"));
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# TUI Design")
        && content.contains("TuiDesign")
        && content.contains("`moyAI`")
        && content.contains("terminal adapter contracts")
        && content.contains("canonical item projection")
        && content.contains("terminal transcript projection")
        && content.contains("permission projection boundary")
        && content.contains("provider projection boundary")
        && content.contains("config projection boundary")
        && content.contains("route-owned verification evidence")
        && content.contains("current closed-network provider boundary")
        && content.contains("historical implementation evidence boundary");
    let stale_authority = [
        "`moyai`",
        "moyai/",
        "Phase6",
        "Phase7",
        "opencode",
        "Roo Code",
        "ratatui",
        "crossterm",
        "tui-textarea",
        "read / list / grep / glob",
        "http://192.168.10.103:1234",
        "qwen/qwen3.6-35b-a3b",
        "実装順序",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn flow_contract_harness_map_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("flow-contract-harness-responsibility-map.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_manual_st_path = content.contains("`moyAI/tests/manual_ST/");
    let has_current_comparison_basis =
        content.contains("Codex") && content.contains("Roo Code") && content.contains("opencode");
    let route_neutral_responsibility = content.contains("representative route")
        && content.contains("evidence source")
        && content.contains("current design authority");
    let stale_lowercase_manual_st_path = content.contains("`moyai/tests/manual_ST/");
    let obsolete_comparison_surface = content.contains("OpenClaw");
    let case_specific_current_authority =
        content.contains("## 6. Case2") || content.contains("Case2 の現在地");
    let exact_python_rerun_authority = content.contains("exact `python -m unittest` rerun")
        || content.contains("exact `python -m unittest` rerun lane");

    has_current_manual_st_path
        && has_current_comparison_basis
        && route_neutral_responsibility
        && !stale_lowercase_manual_st_path
        && !obsolete_comparison_surface
        && !case_specific_current_authority
        && !exact_python_rerun_authority
}

pub fn agent_harness_architecture_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("agent-harness-architecture.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_architecture = content.contains("# Agent Harness Architecture")
        && content.contains("typed content-changing action-family evidence")
        && content.contains("support/context action-family evidence")
        && content.contains("route-owned verification command obligation")
        && content.contains("VerificationRunResult")
        && content.contains("language/test-runner adapter evidence")
        && content.contains("workflow-neutral scenario contract");
    let stale_exact_action_surface = [
        "productive write / patch / read",
        "exact command missing or stale evidence",
        "concrete repair recorded and exact rerun due",
        "python -m unittest failure",
        "Action Routing / exact rerun",
    ]
    .into_iter()
    .any(|stale_authority| content.contains(stale_authority));

    has_current_architecture && !stale_exact_action_surface
}

pub fn agent_harness_components_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("agent-harness-components.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    content.contains("# Agent Harness Components")
        && content.contains("`moyAI`")
        && content.contains("Agent Harness Engine")
        && !content.contains("`moyai`")
}

pub fn agent_state_machine_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("agent-state-machine.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Agent State Machine")
        && content.contains("`moyAI`")
        && content.contains("`moyAI/src/session/state.rs`")
        && content.contains("typed action-family evidence")
        && content.contains("route-owned verification command obligation")
        && content.contains("VerificationRunResult")
        && content.contains("language/test-runner adapter evidence")
        && content.contains("Codex primary comparison")
        && content.contains("Roo Code local-LLM supplement");
    let stale_authority = [
        "`moyai`",
        "`moyai/src/session/state.rs`",
        "`moyai/src/agent/state.rs`",
        "exact command",
        "exact required command",
        "exact rerun",
        "exact verification",
        "concrete repair",
        "write/apply_patch",
        "`write`",
        "`apply_patch`",
        "`read`",
        "`shell`",
        "`py_compile`",
        "`python -m unittest`",
        "python -m unittest",
        "py_compile",
        "OpenClaw",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn agent_harness_implementation_design_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("agent-harness-implementation-design.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Agent Harness Implementation Design")
        && content.contains("`moyAI`")
        && content.contains("`moyAI/src/`")
        && content.contains("typed event-log replay")
        && content.contains("Artifact Registry")
        && content.contains("Contract Registry")
        && content.contains("route-owned evidence")
        && content.contains("deterministic harness contracts")
        && content.contains("closed-network no-provider replay");
    let stale_authority = [
        "`moyai`",
        "moyai/src",
        "moyai schema",
        "moyai replay",
        "moyai contract",
        "Legacy migration",
        "Compatibility",
        "old transcript",
        "legacy case artifact",
        "legacy adapter",
        "pre-policy case2",
        "case2 evidence",
        "provider / shell / workspace mutation",
        "provider or shell",
        "Still intentionally open",
        "remaining open work",
        "latest representative Desktop GUI NG",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn typed_contract_inventory_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("typed-contract-inventory.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_project_name = content.contains("`moyAI`");
    let has_current_manual_st_path = content.contains("`moyAI/tests/manual_ST/");
    let has_provider_neutral_preflight = content.contains("OpenAI-compatible metadata")
        && content.contains("capability evidence")
        && content.contains("provider metadata mode");
    let stale_lowercase_project = content.contains("`moyai`");
    let stale_lowercase_manual_st_path = content.contains("`moyai/tests/manual_ST/");
    let stale_lm_studio_payload = content.contains("LM Studio metadata summary");
    let case_primary_contract_source = content.contains("case*/spec.md");
    let exact_language_specific_verification_command_authority =
        content.contains("exact `py_compile` / `python -m unittest`");
    let typed_verification_command_obligation_evidence =
        content.contains("typed_verification_command_obligation_evidence");

    has_current_project_name
        && has_current_manual_st_path
        && has_provider_neutral_preflight
        && typed_verification_command_obligation_evidence
        && !stale_lowercase_project
        && !stale_lowercase_manual_st_path
        && !stale_lm_studio_payload
        && !case_primary_contract_source
        && !exact_language_specific_verification_command_authority
}

pub fn current_authority_index_current_product_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("current-authority-index.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    content.contains("# Current Authority Index")
        && content.contains("moyAI")
        && content.contains("### SingleControlPlane")
        && content.contains("### ToolLifecycleOwner")
        && content.contains("### ProjectionSeparation")
        && content.contains("### HarnessEvidence")
        && !content.contains("moyai")
}

pub fn codex_lifecycle_conformance_audit_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("codex-lifecycle-conformance-audit.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Lifecycle Conformance Audit")
        && content.contains("CodexLifecycleConformanceAudit")
        && content.contains("current `moyAI`")
        && content.contains("canonical item stream")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("`TurnControlEnvelope`")
        && content.contains("ActionAuthority dispatch surface ownership")
        && content.contains("runtime capability hydration")
        && content.contains("`ToolLifecycleEnvelope`")
        && content.contains("projection separation")
        && content.contains("route obligations model-visible design-visible")
        && content.contains("active preflight gate families")
        && content.contains("historical incident evidence boundary");
    let stale_authority = [
        "`moyai`",
        "Phase12 lifecycle hardening",
        "exact verification rerun",
        "concrete required write dispatch authority",
        "python -m unittest",
        "FR10-2026-05-21-018 Classification",
        "current pre-fix checkpoint",
        "write` / `apply_patch",
        "write / apply_patch",
        "exact `py_compile`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_control_plane_redesign_expanded_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("codex-control-plane-redesign-expanded.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Control-Plane Redesign Expanded Review")
        && content.contains("CodexControlPlaneRedesignExpanded")
        && content.contains("current moyAI")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("TurnControlEnvelope")
        && content.contains("ActionAuthority")
        && content.contains("ProjectionBundle")
        && content.contains("ToolLifecycleEnvelope")
        && content.contains("runtime capability hydration")
        && content.contains("event-sourced runtime")
        && content.contains("route-owned obligations")
        && content.contains("app boundary projection")
        && content.contains("active preflight gate families")
        && content.contains("historical evidence boundary")
        && content.contains("Codex primary comparison")
        && content.contains("Roo Code local-LLM supplement")
        && content.contains("opencode third reference");
    let stale_authority = [
        "`moyai`",
        "current `moyai`",
        "FR2 cluster",
        "case2b",
        "OpenClaw",
        "current tool set: read / write / apply_patch / shell",
        "tool_choice=required",
        "implementation slice",
        "2026-05-04",
        "2026-05-05",
        "old `AgentLoop`",
        "旧 `AgentLoop`",
        "OpenClaw 型",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_derived_redesign_recommendations_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("codex-derived-redesign-recommendations.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex-derived Redesign Recommendations")
        && content.contains("CodexDerivedRedesignRecommendations")
        && content.contains("current `moyAI`")
        && content.contains("historical recommendation boundary")
        && content.contains("adopted protocol-first runtime")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("single control-plane")
        && content.contains("ToolOrchestrator-owned lifecycle")
        && content.contains("protocol store")
        && content.contains("app boundary projection")
        && content.contains("active preflight gate families")
        && content.contains("route-owned verification evidence");
    let stale_authority = [
        "`moyai`",
        "Phase12 live rerun restart point",
        "Phase R1",
        "Phase R2",
        "Phase R3",
        "Phase R4",
        "Phase R5",
        "Phase R6",
        "ThreadOp",
        "TurnEngine",
        "rollout/*.jsonl",
        "thread/start",
        "tool/approval/respond",
        "current tool set",
        "LM Studio metadata",
        "OpenClaw",
        "Roo Code 型",
        "Rebuild vs incremental",
        "次の大きな設計作業",
        "作り直すべき",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_ui_adoption_review_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("codex-ui-adoption-review.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex UI Adoption Review")
        && content.contains("CodexUiAdoptionReview")
        && content.contains("current `moyAI`")
        && content.contains("Desktop/App typed adapter boundary")
        && content.contains("canonical item projection")
        && content.contains("artifact pane")
        && content.contains("diff visibility")
        && content.contains("composer dispatch context")
        && content.contains("left navigation shell")
        && content.contains("local command palette")
        && content.contains("keyboard shortcut overlay")
        && content.contains("non-adopted cloud/account/plugin/realtime boundary")
        && content.contains("historical screenshot evidence boundary");
    let stale_authority = [
        "Date: 2026-05-12",
        "C:/Users/",
        "moyai-feature",
        "Workspace grep exists",
        "Slint",
        "FR10",
        "Implemented on",
        "Follow-up screenshot comparison",
        "GUI screenshots",
        "project_sandbox/manual-st-case1",
        "cargo fmt --all --check",
        "cargo check",
        "cargo test --lib",
        "cargo build --bin moyai-desktop",
        "2026-05-15",
        "2026-05-14",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_lifecycle_fr03_gap_analysis_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("codex-lifecycle-fr03-gap-analysis.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Lifecycle FR03 Gap Analysis")
        && content.contains("CodexLifecycleFr03GapAnalysis")
        && content.contains("current `moyAI`")
        && content.contains("historical FR03 evidence boundary")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("rejected proposal lifecycle")
        && content.contains("candidate repair evidence")
        && content.contains("ToolOrchestrator-owned lifecycle")
        && content.contains("call-id-symmetric tool output")
        && content.contains("typed candidate admission")
        && content.contains("compaction continuity")
        && content.contains("active preflight gate families");
    let stale_authority = [
        "`moyai`",
        "FR03-2026-05-04-001",
        "case1",
        "calculator.py",
        "`ValueError`",
        "`write`",
        "`shell`",
        "`read`",
        "codex/codex-rs/",
        "moyai/src/",
        "OpenClaw-style",
        "Roo Code-style",
        "The next iteration-process steps",
        "Required Core Route A",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_itemlifecycle_survey_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("codex-itemlifecycle-survey.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Item Lifecycle Survey")
        && content.contains("CodexItemLifecycleSurvey")
        && content.contains("current `moyAI`")
        && content.contains("historical incident evidence boundary")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("canonical item stream")
        && content.contains("submitted model action lifecycle")
        && content.contains("call-id-scoped tool output")
        && content.contains("rejected proposal evidence")
        && content.contains("failed edit recovery item")
        && content.contains("final assistant message lifecycle")
        && content.contains("compaction continuity")
        && content.contains("route/harness boundary")
        && content.contains("provider-boundary classification")
        && content.contains("active preflight gate families");
    let stale_authority = [
        "`moyai`",
        "`rg`",
        "grep",
        "FR20-",
        "FR21-",
        "FR22-",
        "case1",
        "case2a",
        "case2c",
        "case3",
        "calculator.py",
        "test_calculator.py",
        "space_invader.py",
        "LM Studio",
        "vLLM-MLX",
        "`apply_patch`",
        "`todowrite`",
        "`write`",
        "`shell`",
        "`read`",
        "python -m unittest",
        "PowerShell",
        "Get-Content",
        "2026-05-",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_reference_comparison_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("codex-reference-comparison.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Reference Comparison")
        && content.contains("CodexReferenceComparison")
        && content.contains("current `moyAI`")
        && content.contains("historical incident evidence boundary")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("single control-plane")
        && content.contains("typed tool lifecycle")
        && content.contains("event-sourced runtime")
        && content.contains("compaction continuity")
        && content.contains("local-LLM recovery boundary")
        && content.contains("harness engineering")
        && content.contains("Codex primary reference")
        && content.contains("Roo Code recovery reference")
        && content.contains("opencode scope reference")
        && content.contains("OpenClaw execution reference");
    let stale_authority = [
        "`moyai`",
        "2026-05-25",
        "vLLM-MLX",
        "LM Studio",
        "FR10",
        "FR20-",
        "FR21-",
        "FR22-",
        "case1",
        "calculator.py",
        "test_calculator.py",
        "loop_impl.rs",
        "`apply_patch`",
        "`write`",
        "`shell`",
        "`read`",
        "unittest",
        "次に更新すべき設計文書",
        "次の修正サイクル",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn codex_structure_map_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("codex-structure-map.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Codex Structure Map")
        && content.contains("CodexStructureMap")
        && content.contains("current `moyAI`")
        && content.contains("historical source evidence boundary")
        && content.contains("protocol boundary")
        && content.contains("turn context")
        && content.contains("canonical item stream")
        && content.contains("tool lifecycle ownership")
        && content.contains("permission and sandbox model")
        && content.contains("compaction continuity")
        && content.contains("memory boundary")
        && content.contains("persistence split")
        && content.contains("app boundary")
        && content.contains("harness engineering")
        && content.contains("non-adopted breadth boundary");
    let stale_authority = [
        "`moyai`",
        "codex/codex-rs/",
        "`Op`",
        "`EventMsg`",
        "`UserTurn`",
        "`ResponseItem`",
        "`TurnItem`",
        "`apply_patch`",
        "`write`",
        "`shell`",
        "`read`",
        "gpt-5",
        "/responses/compact",
        "lmstudio",
        "ollama",
        "まだ protocol-first ではない",
        "agent loop 本体",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn contract_comparison_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("contract-comparison.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Contract Comparison")
        && content.contains("ContractComparison")
        && content.contains("current `moyAI`")
        && content.contains("historical comparison evidence boundary")
        && content.contains("Codex primary contract reference")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("single control-plane")
        && content.contains("canonical item stream")
        && content.contains("typed tool lifecycle")
        && content.contains("Roo Code recovery reference")
        && content.contains("opencode session hardening reference")
        && content.contains("current `moyAI` contract authority")
        && content.contains("closed-network provider boundary")
        && content.contains("route-owned verification evidence")
        && content.contains("harness engineering");
    let stale_authority = [
        "`moyai`",
        "moyai/src/",
        "AgentLoop",
        "loop_impl.rs",
        "SessionPrompt.runLoop",
        "presentAssistantMessage",
        "validateToolUse",
        "ToolRepetitionDetector",
        "attempt_completion",
        "case1",
        "case2",
        "case3",
        "project_sandbox/manual-st",
        "calculator.py",
        "test_calculator.py",
        "space_invader.py",
        "python -m unittest",
        "unittest",
        "LM Studio",
        "vLLM-MLX",
        "`todowrite`",
        "`write`",
        "`shell`",
        "`read`",
        "DOOM_LOOP_THRESHOLD",
        "2026-04-",
        "2026-05-",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn harness_comparison_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("harness-comparison.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Harness Comparison")
        && content.contains("HarnessComparison")
        && content.contains("current `moyAI`")
        && content.contains("historical harness evidence boundary")
        && content.contains("Codex primary harness reference")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("canonical item stream")
        && content.contains("active preflight gate families")
        && content.contains("deterministic replay")
        && content.contains("route-owned evidence")
        && content.contains("Roo Code recovery harness reference")
        && content.contains("opencode session harness reference")
        && content.contains("current `moyAI` harness authority")
        && content.contains("closed-network provider boundary")
        && content.contains("manual ST evidence boundary")
        && content.contains("harness engineering");
    let stale_authority = [
        "`moyai`",
        "moyai/src/",
        "moyai/tests/manual_ST/case",
        "case1",
        "case2",
        "case3",
        "case4",
        "case5",
        "case6",
        "case7",
        "project_sandbox/manual-st",
        "python -m unittest",
        "unittest",
        "py_compile",
        "LM Studio",
        "vLLM-MLX",
        "bun:test",
        "vitest",
        "stream-json",
        "attempt_completion",
        "update_todo_list",
        "validateToolUse",
        "ToolRepetitionDetector",
        "`todowrite`",
        "`write`",
        "`shell`",
        "`read`",
        "tool_choice=required",
        "write.path",
        "image_count",
        "image_bytes",
        "space_invader.py",
        "calculator.py",
        "test_calculator.py",
        "README.md",
        "grep-based",
        "2026-04-",
        "2026-05-",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn opencode_structure_map_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("opencode-structure-map.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# opencode Structure Map")
        && content.contains("OpencodeStructureMap")
        && content.contains("current `moyAI`")
        && content.contains("historical source evidence boundary")
        && content.contains("opencode session engine reference")
        && content.contains("opencode tool registry reference")
        && content.contains("opencode workspace policy reference")
        && content.contains("opencode storage and event reference")
        && content.contains("opencode provider capability reference")
        && content.contains("current `moyAI` opencode reference authority")
        && content.contains("Codex-style lifecycle alignment")
        && content.contains("closed-network adoption boundary")
        && content.contains("non-adopted ecosystem boundary");
    let stale_authority = [
        "`moyai`",
        "Phase2",
        "Phase3",
        "Phase5",
        "Phase6",
        "Phase7",
        "opencode/packages/opencode/src/",
        "opencode/packages/opencode/bin/opencode",
        "src/tool/grep.ts",
        "src/tool/bash.ts",
        "src/tool/read.ts",
        "src/tool/apply_patch.ts",
        "src/session/prompt.ts",
        "src/session/processor.ts",
        "src/provider/provider.ts",
        "src/provider/models.ts",
        "src/file/index.ts",
        "grep",
        "Hono",
        "yargs",
        "Node",
        "plugin system",
        "MCP",
        "現行仕様では採用しない",
        "現行仕様では対象外",
        "CLI 中心",
        "将来候補",
        "保留",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn opencode_flow_description_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("flow_description_opencode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# opencode Flow Description")
        && content.contains("OpencodeFlowDescription")
        && content.contains("current `moyAI`")
        && content.contains("historical flow evidence boundary")
        && content.contains("opencode session loop reference")
        && content.contains("user input normalization")
        && content.contains("tool surface resolution")
        && content.contains("stream processor itemization")
        && content.contains("retry and compaction boundary")
        && content.contains("subtask session boundary")
        && content.contains("todo progress projection boundary")
        && content.contains("completion boundary")
        && content.contains("Codex-style flow alignment")
        && content.contains("current `moyAI` flow authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "opencode/packages/opencode/",
        "src/session/",
        "src/tool/",
        "src/cli/",
        "`todowrite`",
        "`apply_patch`",
        "`edit`",
        "`write`",
        "`codesearch`",
        "`websearch`",
        "DOOM_LOOP_THRESHOLD",
        "case5",
        "case",
        "2026-04-",
        "LM Studio",
        "Roo Code",
        "GPT",
        "Claude",
        "Google",
        "beast",
        "anthropic",
        "OPENCODE",
        "MCP",
        "plugin",
        "Read tool",
        "provider family",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn roocode_flow_description_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("flow_description_roocode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Roo Code Flow Description")
        && content.contains("RooCodeFlowDescription")
        && content.contains("current `moyAI`")
        && content.contains("historical flow evidence boundary")
        && content.contains("Roo Code recovery-flow reference")
        && content.contains("task state loop reference")
        && content.contains("model-visible recovery state")
        && content.contains("todo reinjection boundary")
        && content.contains("completion gate boundary")
        && content.contains("repetition guard boundary")
        && content.contains("approval category boundary")
        && content.contains("feedback loop boundary")
        && content.contains("Codex-style control-plane alignment")
        && content.contains("current `moyAI` recovery-flow authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "Roo-Code/",
        "src/",
        "`update_todo_list`",
        "`attempt_completion`",
        "ToolRepetitionDetector",
        "AttemptCompletionTool",
        "UpdateTodoListTool",
        "ClineProvider",
        "Task.ts",
        "case5",
        "2026-04-",
        "LM Studio",
        "MCP",
        "reserved",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn moyai_flow_description_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("flow_description_moyai.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# moyAI Flow Description")
        && content.contains("MoyaiFlowDescription")
        && content.contains("current `moyAI`")
        && content.contains("historical flow evidence boundary")
        && content.contains("current runtime flow authority")
        && content.contains("Thread / Turn / Item protocol")
        && content.contains("single control-plane")
        && content.contains("canonical item stream")
        && content.contains("state reducer authority")
        && content.contains("ToolOrchestrator lifecycle")
        && content.contains("prompt projection boundary")
        && content.contains("verification lane boundary")
        && content.contains("completion and handoff boundary")
        && content.contains("compaction continuity boundary")
        && content.contains("active preflight gate families")
        && content.contains("closed-network provider boundary");
    let stale_authority = [
        "`moyai`",
        "moyai/src/",
        "src/",
        "AgentLoop",
        "loop_impl.rs",
        "`todowrite`",
        "`shell`",
        "`write`",
        "case1",
        "case2",
        "case5",
        "case7",
        "2026-04-",
        "project_sandbox/manual-st",
        "space_invader.py",
        "python -m",
        "LM Studio",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn opencode_contract_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("contract_opencode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# opencode Contract")
        && content.contains("OpencodeContract")
        && content.contains("current `moyAI`")
        && content.contains("historical contract evidence boundary")
        && content.contains("opencode session contract reference")
        && content.contains("session loop contract")
        && content.contains("stream item persistence")
        && content.contains("retry visibility contract")
        && content.contains("compaction replay boundary")
        && content.contains("permission rules boundary")
        && content.contains("todo persistence boundary")
        && content.contains("loop safety boundary")
        && content.contains("Codex-style contract alignment")
        && content.contains("current `moyAI` opencode contract reference authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "opencode/packages/opencode/",
        "src/session/",
        "src/tool/",
        "src/agent/",
        "`todowrite`",
        "SessionPrompt",
        "SessionProcessor",
        "SessionRetry",
        "SessionCompaction",
        "case5",
        "2026-04-",
        "LM Studio",
        "prompt family",
        "読む順番",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn roocode_contract_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("contract_roocode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Roo Code Contract")
        && content.contains("RooCodeContract")
        && content.contains("current `moyAI`")
        && content.contains("historical contract evidence boundary")
        && content.contains("Roo Code recovery contract reference")
        && content.contains("task state ownership boundary")
        && content.contains("per-turn environment reinjection")
        && content.contains("todo persistence contract")
        && content.contains("completion gate contract")
        && content.contains("tool validation boundary")
        && content.contains("repetition guard boundary")
        && content.contains("multimodal input boundary")
        && content.contains("approval category reference")
        && content.contains("Codex-style contract alignment")
        && content.contains("current `moyAI` Roo Code recovery contract authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "Roo-Code/",
        "src/core/",
        "`update_todo_list`",
        "`attempt_completion`",
        "Task.ts",
        "UpdateTodoListTool",
        "AttemptCompletionTool",
        "ToolRepetitionDetector",
        "validateToolUse",
        "LM Studio",
        "2026-04-",
        "読む順番",
        "read / write / execute",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn opencode_verification_harness_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("verification-harness_opencode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# opencode Verification Harness")
        && content.contains("OpencodeVerificationHarness")
        && content.contains("current `moyAI`")
        && content.contains("historical harness evidence boundary")
        && content.contains("opencode deterministic harness reference")
        && content.contains("isolated project fixture boundary")
        && content.contains("fake provider control boundary")
        && content.contains("component session harness boundary")
        && content.contains("tool and permission harness boundary")
        && content.contains("server route harness boundary")
        && content.contains("subsystem split boundary")
        && content.contains("deterministic runtime evidence")
        && content.contains("Codex-style harness alignment")
        && content.contains("current `moyAI` opencode deterministic harness authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "opencode/packages/opencode/test/",
        "test/fixture/fixture.ts",
        "test/fake/provider.ts",
        "test/lib/llm-server.ts",
        "prompt-effect.test.ts",
        "processor-effect.test.ts",
        "compaction.test.ts",
        "retry.test.ts",
        "structured-output-integration.test.ts",
        "session-actions.test.ts",
        "permission-task.test.ts",
        "task.test.ts",
        "`apply_patch`",
        "`write`",
        "`bash`",
        "`read`",
        "SessionPrompt",
        "SessionProcessor",
        "case1",
        "case3",
        "case5",
        "2026-04-",
        "読む順番",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn roocode_verification_harness_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("verification-harness_roocode.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Roo Code Verification Harness")
        && content.contains("RooCodeVerificationHarness")
        && content.contains("current `moyAI`")
        && content.contains("historical harness evidence boundary")
        && content.contains("Roo Code stream harness reference")
        && content.contains("CLI stream control-plane boundary")
        && content.contains("tool contract harness boundary")
        && content.contains("task runtime harness boundary")
        && content.contains("completion and todo discipline")
        && content.contains("dispatch guard boundary")
        && content.contains("repetition guard harness boundary")
        && content.contains("local-LLM recovery harness reference")
        && content.contains("Codex-style harness alignment")
        && content.contains("current `moyAI` Roo Code recovery harness authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "Roo-Code/",
        "apps/cli/scripts",
        "src/core/",
        "stream-harness.ts",
        "run.ts",
        "attemptCompletionTool.spec.ts",
        "updateTodoListTool.spec.ts",
        "validateToolUse.spec.ts",
        "ToolRepetitionDetector.spec.ts",
        "Task.spec.ts",
        "grace-retry-errors.spec.ts",
        "followup-during-streaming.ts",
        "followup-after-completion.ts",
        "cancel-active-task.ts",
        "start-while-busy.ts",
        "multi-message-queue-order.ts",
        "runStreamCase",
        "stdin-prompt-stream",
        "stream-json",
        "`attempt_completion`",
        "`update_todo_list`",
        "<user_message>",
        "`Task`",
        "VS Code API mock",
        "case5",
        "2026-04-",
        "LM Studio",
        "project_sandbox",
        "読む順番",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn openclaw_runtime_survey_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("surveys")
                .join("openclaw-runtime-survey.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# OpenClaw Runtime Survey")
        && content.contains("OpenClawRuntimeSurvey")
        && content.contains("current `moyAI`")
        && content.contains("historical runtime evidence boundary")
        && content.contains("OpenClaw tool lifecycle reference")
        && content.contains("tool surface composition boundary")
        && content.contains("execution contract boundary")
        && content.contains("owned tool runtime boundary")
        && content.contains("run-scoped loop detection boundary")
        && content.contains("workspace file boundary")
        && content.contains("exec lifecycle boundary")
        && content.contains("Codex app-server adapter boundary")
        && content.contains("projection isolation boundary")
        && content.contains("Codex-style runtime alignment")
        && content.contains("current `moyAI` OpenClaw runtime reference authority")
        && content.contains("closed-network adoption boundary");
    let stale_authority = [
        "`moyai`",
        "openclaw/",
        "openclaw/src/",
        "extensions/codex",
        "package.json",
        "openclaw.mjs",
        "createOpenClawCodingTools",
        "before_tool_call",
        "after_tool_call",
        "strict-agentic",
        "`update_plan`",
        "`runId`",
        "@mariozechner",
        "GPT-5",
        "qwen",
        "FR-080",
        "case2",
        "Case2LaneInvariantAudit",
        "tool-loop-detection.ts",
        "apply-patch.ts",
        "bash-tools.exec-runtime.ts",
        "run-attempt.ts",
        "event-projector.ts",
        "read/write/apply_patch",
        "調査根拠として読んだ",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority && !stale_authority
}

pub fn replay_first_harness_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("replay-first-harness.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    content.contains("# Replay-first Harness Design")
        && content.contains("`moyAI`")
        && content.contains("route taxonomy")
        && content.contains("route-owned replay evidence")
        && content.contains("invariant replay fixture")
        && ![
            "`moyai`",
            "future `moyai/` schema modules",
            "manual-st-20260421-case2",
            "manual_ST.case2",
            "Desktop GUI e2e を case1 から再開",
            "Case2 supplies a current representative fixture",
            "latest known stopper",
            "Expected classification for the latest known stopper",
            "exact unittest rerun",
            "collision behavior lacks shared public contract",
            "pre-policy case2 evidence",
        ]
        .into_iter()
        .any(|stale_authority| content.contains(stale_authority))
}

pub fn run_store_event_log_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("run-store-event-log-design.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    content.contains("# Run Store / Event Log / Registry Split Design")
        && content.contains("RunStoreEventLogDesign")
        && content.contains("`moyAI`")
        && content.contains("Agent Harness Engine")
        && content.contains("typed feedback envelope")
        && content.contains("typed required action projection")
        && content.contains("route-owned verification command contract")
        && content.contains("route taxonomy E2E gate")
        && ![
            "`moyai`",
            "Required action、result hash、model-visible correction",
            "representative scenario の behavior-level blocker",
            "exact `python -m unittest` rerun lane",
            "final Desktop GUI route proof",
            "FR-099",
            "unavailable `read`",
            "route-owned behavior blocker",
            "verification rerun lane",
            "failing unittest stdout / stderr",
        ]
        .into_iter()
        .any(|stale_authority| content.contains(stale_authority))
}

pub fn tiered_quality_gates_route_taxonomy_invariant_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("tiered-quality-gates.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("route taxonomy")
        && content.contains("route-owned artifact evidence")
        && content.contains("invariant / artifact-role authority")
        && content.contains("typed lifecycle evidence")
        && content.contains("adapter-owned verification evidence")
        && content.contains("user-overridden model gate and fresh rerun boundary")
        && content.contains("gate-family ordering");
    let stale_authority = [
        "fresh representative behavior stopper",
        "representative route starts from case1",
        "all required cases pass",
        "legacy case artifact maps",
        "case artifact maps to expected gate results",
        "exact verification rerun",
        "exact verification rerun まで到達",
        "provider request tools",
        "tool_choice=required",
        "verification lane で non-equivalent shell",
        "representative Desktop GUI route",
        "Desktop GUI e2e result",
        "model availability gate result",
        "configured model and required metadata available",
        "manual ST の特定 case",
        "shared `QualityGateResult` model and enums",
    ]
    .into_iter()
    .any(|stale_authority| content.contains(stale_authority));

    has_current_authority && !stale_authority
}

pub fn thread_turn_item_protocol_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("thread-turn-item-protocol.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_project_name = content.contains("`moyAI`");
    let has_current_protocol_path = content.contains("`moyAI/src/protocol/`");
    let has_typed_required_action_authority =
        content.contains("RequiredAction` typed fields") && content.contains("renderer output");
    let has_action_authority_surface_split =
        content.contains("ActionAuthority owns provider dispatch surface");
    let has_runtime_capability_hydration = content.contains("runtime capability hydration");
    let has_current_tool_lifecycle_authority = content.contains(
        "`ToolLifecycleEnvelope` は current protocol 型として ToolOrchestrator-owned lifecycle snapshot を表す。",
    );
    let has_active_control_envelope_gate =
        content.contains("preflight `preflight.control_envelope.dispatch_projection_authority`");
    let has_historical_implementation_boundary =
        content.contains("historical implementation evidence boundary");
    let has_current_connection_status = content.contains("## 12. Current authority map");
    let stale_lowercase_project = content.contains("`moyai`");
    let stale_lowercase_protocol_path = content.contains("`moyai/src/protocol/`");
    let string_grammar_dispatch_authority = content
        .contains("`write:<target>` / `shell:<command>` などの executable action を示す場合");
    let stale_active_contract_surface_equality = content.contains(
        "`TurnContext.allowed_tools` と `TurnContext.active_contract.allowed_tools` は同じ surface",
    );
    let stale_turn_context_model_capability_image_authority =
        content.contains("`TurnContext.model_capabilities.supports_images=true`");
    let stale_implementation_rollout_authority = content.contains("R1 / R2 の Rust 実装は")
        || content.contains("unit test は同 module 内に置き")
        || content.contains("この順序を逆にしない");
    let stale_phase_skeleton_authority = content.contains("R1 skeleton")
        || content.contains("R4 で ToolRouter / ToolOrchestrator へ移行");
    let stale_control_envelope_gate = content.contains("control_envelope_projection_consistency");
    let stale_future_connection_order = content.contains("## 12. R2 以降の接続順")
        || content.contains("`TurnEngine` skeleton")
        || content.contains("ToolRouter / ToolOrchestrator migration");

    has_current_project_name
        && has_current_protocol_path
        && has_typed_required_action_authority
        && has_action_authority_surface_split
        && has_runtime_capability_hydration
        && has_current_tool_lifecycle_authority
        && has_active_control_envelope_gate
        && has_historical_implementation_boundary
        && has_current_connection_status
        && !stale_lowercase_project
        && !stale_lowercase_protocol_path
        && !string_grammar_dispatch_authority
        && !stale_active_contract_surface_equality
        && !stale_turn_context_model_capability_image_authority
        && !stale_implementation_rollout_authority
        && !stale_phase_skeleton_authority
        && !stale_control_envelope_gate
        && !stale_future_connection_order
}

pub fn turn_decision_pipeline_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("turn-decision-pipeline.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_product_authority = content.contains("`moyAI` の manual ST failure")
        || content.contains("current `moyAI` Turn Decision Pipeline");
    let has_turn_control_authority = content.contains("`TurnControlEnvelope`")
        && content.contains("`ActionAuthority`")
        && content.contains("`ProjectionBundle`");
    let has_typed_lifecycle_authority = content.contains("typed lifecycle authority")
        && content.contains("action-family authority")
        && content.contains("adapter-owned evidence")
        && content.contains("workflow-neutral invariant family")
        && content.contains("provider dispatch ownership")
        && content.contains("historical FR evidence boundary");
    let stale_lowercase_product_authority = [
        "`moyai` の manual ST failure",
        "current `moyai` Turn Decision Pipeline",
        "`moyai` provider dispatch",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_exact_fr_language_authority = [
        "FR2-",
        "FR03-",
        "FR-",
        "NO TESTS RAN",
        "NameError",
        "`write:<source>`",
        "`write:<generated-test>`",
        "`shell:<command>`",
        "typed required action projection=write:",
        "typed required action projection=shell:",
        "python -m unittest",
        "unittest",
        "projectile movement",
        "bounds lifecycle",
        "spawn allowance",
        "SCORE_PER_ROW",
        "space_invader",
        "calculator",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_product_authority
        && has_turn_control_authority
        && has_typed_lifecycle_authority
        && !stale_lowercase_product_authority
        && !stale_exact_fr_language_authority
}

pub fn root_specs_current_product_path_authority_fixture_passes() -> bool {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent();
    let Some(root) = root else {
        return false;
    };
    let Ok(readme) = fs::read_to_string(root.join("README.md")) else {
        return false;
    };
    let Ok(project_brief) = fs::read_to_string(root.join("ProjectBrief.md")) else {
        return false;
    };

    let readme_has_current_product = readme.contains("# moyAI");
    let readme_has_current_implementation_path = readme
        .contains("└─ moyAI/            # Rust 実装先")
        && readme.contains("- `moyAI/` 以下の Rust コード");
    let project_brief_has_current_product = project_brief.contains("# ProjectBrief: moyAI");
    let project_brief_has_current_paths = project_brief.contains("`CodingAgent/moyAI`")
        && project_brief.contains("`moyAI/tests/manual_ST`");
    let project_brief_has_current_contract_authority = project_brief
        .contains("current `moyAI` を広義 Agent Harness として捉える全体アーキテクチャ図")
        && project_brief.contains("current `moyAI` の runtime contract 正本")
        && project_brief.contains("current `moyAI` の verification harness 正本");

    let stale_root_spec_authority = [
        "└─ moyai/            # Rust 実装先",
        "- `moyai/` 以下の Rust コード",
        "# ProjectBrief: moyai",
        "`CodingAgent/moyai`",
        "current `moyai`",
        "`moyai/tests/manual_ST`",
    ]
    .into_iter()
    .any(|stale_phrase| readme.contains(stale_phrase) || project_brief.contains(stale_phrase));

    readme_has_current_product
        && readme_has_current_implementation_path
        && project_brief_has_current_product
        && project_brief_has_current_paths
        && project_brief_has_current_contract_authority
        && !stale_root_spec_authority
}

pub fn runtime_contracts_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("runtime-contracts.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority_line = content
        .contains("本書は current build の `moyAI` における runtime contract の正本である。");
    let has_current_state_contract_scope = content.contains("current-state の契約");
    let current_basis_section = content
        .split_once("主な根拠:")
        .and_then(|(_, rest)| rest.split_once("## 1.2").map(|(section, _)| section))
        .unwrap_or("");
    let has_current_owner_paths = [
        "`moyAI/src/agent/state.rs`",
        "`moyAI/src/agent/prompt.rs`",
        "`moyAI/src/agent/loop_impl.rs`",
        "`moyAI/src/agent/lifecycle_kernel.rs`",
        "`moyAI/src/agent/tool_orchestrator.rs`",
        "`moyAI/src/agent/verification.rs`",
        "`moyAI/src/agent/tool_result_classification.rs`",
        "`moyAI/src/tool/todo_write.rs`",
    ]
    .into_iter()
    .all(|owner_path| current_basis_section.contains(owner_path));
    let stale_current_build_product_authority = content
        .contains("本書は current build の `moyai` における runtime contract の正本である。");
    let stale_current_owner_path_authority = current_basis_section.contains("`moyai/src/");
    let stale_normative_body_product_authority = content.contains("`moyai` では");
    let stale_side_product_authority = content.contains("`moyai` 側");
    let stale_unbackticked_body_product_authority = [
        "`moyai` も",
        "`moyai` でも",
        "`moyai` は",
        "`moyai` の",
        "moyai も",
        "moyai でも",
        "moyai は",
        "moyai 内部",
        "moyai shell bootstrap",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_english_body_product_authority = [
        "`moyai` should",
        "`moyai` must",
        "`moyai` also",
        "`moyai` does",
        "`moyai` uses",
        "`moyai` preserves",
        "aligns `moyai` with Codex",
        "current `moyai`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_widget_docs_authority = content.contains("`docs/widget-design.md`")
        || content.contains("required_targets = docs/widget-design.md")
        || content.contains("C:/workspace/project/docs/widget-design.md");
    let stale_widget_test_artifact_authority = [
        "`widget.spec`",
        "`src/widget.spec.tsx`",
        "`widget.spec.tsx`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_requested_work_specific_example_authority = [
        "`README.md` を `in_progress`、`space_invader.py`",
        "`docs/calculator-design.md の仕様に合わせて calculator.py / test_calculator.py を更新`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_active_target_repair_specific_example_authority = [
        "`typed required action projection=write:test_calculator.py`",
        "`write:space_invader.py`",
        "`test_calculator.py` が active target",
        "`docs/calculator-design.md` が model-facing scope",
        "`calculator.py` への stale edit",
        "`calculator.py` full rewrite",
        "active target の `test_calculator.py` authoring",
        "`space_invader.py` のような implementation target",
        "`Game.state` や `SpaceInvaderGame._player_direction`",
        "`game.score` などの failing public state fields",
        "`game.state` expected `GameState.WIN` observed `GameState.PLAYING`",
        "`game.state terminal transition to GameState.WIN`",
        "Space Invader 固有 rule",
        "`calculate(left, operator, right)`",
        "`calculate(expression)`",
        "`calculator.py` と `test_calculator.py`",
        "`calculator.py` / `test_calculator.py`",
        "python calculator.py",
        "`calculate(",
        "`test_calculator.py` だけへ rotation",
        "evidence.target = test_widget.py",
        "for case1 to pass, then exposed a different case3 stage3 failure",
        "Required Core Route A case1. The rerun then exposed a different case3 stage3 failure",
        "rerun stopped in case1 before GUI",
        "generated `test_calculator.py` contained",
        "`RepairOperationTemplate.exact_target=calculator.py`",
        "Case1 passed, proving",
        "GUI rerun reached case3 stage3",
        "GUI case1 failure where `test_calculator.py`",
        "imports `unittest` and the production module",
        "one or more `Test*` `unittest.TestCase`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_route_specific_example_authority = [
        "`basic_design.md` の topic 不足",
        "stale な `detail_design.md`",
        "`detail_design.md` の `data model` topic",
        "`test_calculator.py` todo",
        "`space_invader.py`、`test_space_invader.py`、`README.md`",
        "`space_invader.py` / `test_space_invader.py` / `README.md`",
        "`design.md` を変更せず `app.py` / `test_app.py`",
        "`scenario_contract.md/json` を変更せず `space_invader.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_audit_specific_path_authority = [
        "`docs.md` の reread / rewrite",
        "拒否された `./data/...`",
        "`data/` 配下に `memory/`、`reports/`、`documents/`",
        "BasicDesign の audit feedback",
        "`backend/app/services`",
        "`backend/app/agents`",
        "`backend/app/application`",
        "`backend/app/domain`",
        "`backend/app/simulation`",
        "`backend/app/infrastructure`",
        "`backend/app/api`",
        "`backend/app/core`",
        "exact `write:test_<module>.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_verification_repair_language_case_authority = [
        "ad hoc `python -c ...`",
        "`python -m unittest -v`",
        "canonical command identity として\r\n`python -m unittest`",
        "canonical command identity として\n`python -m unittest`",
        "`python -m py_compile",
        "python -m unittest の実行結果",
        "`python tool.py status`",
        "`python tool.py <mode> <input>`",
        "UTF-8-safe wrappers such as `python -X utf8 -m",
        "unittest` can satisfy `python -m unittest`",
        "When a unittest line asserts",
        "self.assertIn(\"error\", result.stderr)",
        "manual-st-20260421",
        "`space_invader.py`",
        "`test_space_invader.py`",
        "`typed required action projection=shell:python -m unittest`",
        "`python -X utf8 -m unittest`. Generated tests asserted",
        "`ImportError: cannot import name 'X' from 'module'`",
        "`class X`",
        "`def X`",
        "`module.py`",
        "`NameError: name 'x' is not defined`",
        "`enemy_bullet`",
        "`bullet`",
        "`test_enemy_bullet_removed_when_off_screen`",
        "`File \"test_*.py\"`",
        "`assertEqual`",
        "`assertAlmostEqual`",
        "`log` / `sin` / `cos`",
        "generated unittest",
        "`assertRaises`",
        "`assertRaisesRegex`",
        "`ValueError`、`ZeroDivisionError`、`TypeError`",
        "`SyntaxError`、`IndentationError`、`TabError`",
        "`python -m unittest` が `NO TESTS RAN`",
        "失敗した `python -m unittest` の output",
        "`python -m unittest` が成功しても",
        "`unittest` import",
        "`unittest.TestCase` subclass",
        "concrete `def test_*` methods",
        "active target が `*.py`",
        "Python module code only",
        "`SyntaxError` / `IndentationError` / `TabError`",
        "`unittest.loader._FailedTest.<generated_test_module>`",
        "`Failed to import test module`",
        "`ImportError: Failed to import test module`",
        "`NameError: name 'X' is not defined`",
        "generated test の NameError self-defect",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_calculator_glob_authority = content.contains("Bare patterns such as `calculator.py`");
    let stale_later_repair_verification_glob_language_authority = [
        "repair `test_widget.py`",
        "names\r\n`widget.py` as exact target",
        "names\n`widget.py` as exact target",
        "must not show `test_widget.py` in the repair lane",
        "top-level active work names `widget.py`",
        "`source_parse_defect` evidence that targets a `test_*.py` / `*_test.py` artifact",
        "commands\r\n  embedded after cwd setup wrappers such as `cd ... &&` or `cd ... ;`",
        "commands\n  embedded after cwd setup wrappers such as `cd ... &&` or `cd ... ;`",
        "Normalize Python unittest variants with `-X utf8`",
        "Bare patterns such as `workflow.py`",
        "test_widget.py",
        "workflow.py",
        "test_workflow.py",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let python_utf8_env = ["PYTHON", "UTF8"].concat();
    let python_io_encoding_env = ["PYTHON", "IOENCODING"].concat();
    let stale_route_verification_environment_file_example_authority = [
        "generated Python subprocess tests",
        "Windows Shift_JIS / CP932",
        "must not branch on `calculator.py`,",
        "`test_calculator.py`, `subprocess`, or one provider",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase))
        || content.contains(&python_utf8_env)
        || content.contains(&python_io_encoding_env);
    let stale_public_output_subprocess_language_authority = [
        "Python source/test naming contract",
        "`calculator.py`, `test_calculator.py`, stderr wording",
        "`read`, `calculator.py`, one unittest command",
        "`calculator.py`, `__main__`, one unittest assertion",
        "Generated subprocess tests have an executable artifact contract",
        "generated Python test",
        "`CompletedProcess.stdout`",
        "`CompletedProcess.stderr`",
        "`capture_output=True`",
        "`stdout=subprocess.PIPE`",
        "`stderr=subprocess.PIPE`",
        "`TypeError: argument of type 'NoneType' is not iterable`",
        "`result.stdout` / `result.stderr`",
        "Parent unittest stdout",
        "`calculator.py`, Japanese output text",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_content_shape_language_path_authority = [
        "apply_patch` against a Python generated test target",
        "`python_test_module_content_shape`",
        "`C://Users//...`",
        "`C:/workspace/route2/...` is not inside `C:/workspace/route`",
        "one calculator file",
        "both `target.py` and `C:/workspace/.../target.py`",
        "Python generated-test targets",
        "not keyed by `unittest`, calculator",
        "for a Python source target",
        "Python-source content-shape",
        "`docs/calculator-design.md`, Required Core case3",
        "generated-test, Python-source, and readable text artifact content-shape",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_evidence_language_tool_path_authority = [
        "flat conventional test files such as `test_*.py`, `*_test.py`",
        "Japanese `テスト`,\r\n  `unittest`, and equivalent test markers",
        "Japanese `テスト`,\n  `unittest`, and equivalent test markers",
        "Python source write repair projects executable content shape",
        "active Python source target",
        "effective Python source text",
        "`python_source_executable_content_shape`",
        "moyai/tests/manual_ST/*",
        "after one source\r\nsymbol grep",
        "after one source\nsymbol grep",
        "`read(test_*.py)`",
        "grep match line from a test path",
        "`grep(\"def calculate\")`",
        "`read` / `grep` / `docling_convert` / `mcp_call`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_provider_supporting_tool_surface_authority = [
        "`list`, `read`, `grep`, `inspect_directory`, `todowrite`, and similar supporting tools",
        "`inspect_directory`, `list`, `glob`, empty `grep`",
        "provider requests also omit `list`, `read`, `grep`,",
        "`glob`, `inspect_directory`, Docling, MCP, and skill JSON tools",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_surface_exact_tool_authority = [
        "`read`, `grep`, `docling_convert`, and `mcp_call` may gather grounding evidence",
        "non-Codex JSON discovery tools such as `list`, `glob`, and `inspect_directory`",
        "scoped grounding tools `read`, `grep`, `docling_convert`, and `mcp_call`",
        "non-Codex JSON discovery tools (`list`, `glob`, `inspect_directory`), `skill`, and whole-file JSON `write`",
        "`apply_patch`, `shell`, `todowrite`, and scoped grounding tools",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_recovery_grounding_exact_tool_authority = [
        "Normal Code Author / Repair dispatch exposes `apply_patch`, `shell`, and `todowrite`",
        "then narrowed to exactly `apply_patch` and `write`",
        "the next Code Author recovery narrows to `apply_patch` / `write`",
        "Normal Docs Author dispatch exposes `apply_patch`, `shell`, `todowrite`, and scoped grounding",
        "rebuilds exactly `apply_patch` / `write` from stable schemas",
        "provider-visible tools include bounded source-reference `read`",
        "plus `write` / `apply_patch`",
        "Codex-style `shell` and `todowrite` may remain visible as side-channels",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_edit_surface_language_exact_authority = [
        "A non-test Python source target accepts content-changing progress only",
        "Python source content-shape rejects uncommented raw prose lines",
        "kept\r\n  to `apply_patch` / `write` for this recovery",
        "kept\n  to `apply_patch` / `write` for this recovery",
        "A content-changing `write` / `apply_patch` submitted for a non-active target",
        "Whole-file JSON `write(path, content)` is\r\nnot part of the provider-visible normal Code Author surface",
        "Whole-file JSON `write(path, content)` is\nnot part of the provider-visible normal Code Author surface",
        "preserve `apply_patch` as the satisfying edit\r\n  primitive",
        "preserve `apply_patch` as the satisfying edit\n  primitive",
        "must not restore `write` from stable schemas",
        "Target-scoped `read` may be present only while grounding is required",
        "`shell` and `todowrite` may\r\n  remain provider-visible",
        "`shell` and `todowrite` may\n  remain provider-visible",
        "Singleton missing generated-test targets project `apply_patch:<target>`",
        "do not project\r\n  `write:<target>` or named write tool choice",
        "do not project\n  `write:<target>` or named write tool choice",
        "`provider_required_tool_choice_final_message_recovery_active` may narrow the effective surface to\r\n  `write`",
        "`provider_required_tool_choice_final_message_recovery_active` may narrow the effective surface to\n  `write`",
        "`ToolChoice::Named(Write)`",
        "wrong-target recovery narrows to\r\n  `apply_patch` / `write`",
        "wrong-target recovery narrows to\n  `apply_patch` / `write`",
        "preserves bounded `read` together with\r\n  `apply_patch` / `write`",
        "preserves bounded `read` together with\n  `apply_patch` / `write`",
        "The composed recovery still excludes `shell`, broad discovery",
        "For every generated-test target, `write` and projected post-`apply_patch` content",
        "Python / unittest class-base rejection is an adapter-specific specialization",
        "for `test_*.py` /\r\n  `*_test.py` targets",
        "for `test_*.py` /\n  `*_test.py` targets",
        "Python `LanguageEvidenceAdapter` may reject `Test*` class definitions",
        "unittest `TestCase` type",
        "The rejection is a side-effect-free `Required write content shape mismatch`",
        "Public command subprocess coverage remains a separate generated-test contract",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_grounding_shell_text_exact_surface = [
        "After a no-progress plan projection, the recovery surface keeps bounded grounding tools",
        "(`read`, `grep`, `shell`, `docling_convert`, `mcp_call`) plus `apply_patch`",
        "removes `todowrite` from the executable surface",
        "withholds broad `write` until content-bearing grounding evidence exists",
        "PowerShell text cmdlets (`Get-Content`, `Set-Content`, `Out-File`, `Tee-Object`",
        "`Write-Output`) classify as text-producing / text-consuming commands",
        "`Get-Content ... -Encoding UTF8` read is valid grounding evidence",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_runner_repair_exact_surface_authority = [
        "public command coverage 用 subprocess は production command / helper invocation に限る。同じ unittest test module",
        "Python unittest / pytest、JavaScript jest / vitest、Rust cargo test、Go / .NET / JVM",
        "`python` / `node` / `cargo` / `npm` などの runner",
        "public command coverage の target は `ArtifactRole::Test` であり、Python test module だけではない",
        "Python subprocess / unittest は generated-test evidence adapter の実装例",
        "repair prompt は `VerificationRepairPromptProjection` を作り、active target の `ArtifactRole::Test` と\r\n  `LanguageFamily::Python`",
        "repair prompt は `VerificationRepairPromptProjection` を作り、active target の `ArtifactRole::Test` と\n  `LanguageFamily::Python`",
        "`unittest/pytest`、`subprocess argv`",
        "executable `apply_patch` / `write` item 自体",
        "failed inactive `write` / `apply_patch` call",
        "`patch_text` / `content` は provider-visible executable history として出さない",
        "broad whole-file `write(path, content)`",
        "wrong-target authoring recovery は provider-visible executable edit surface を `apply_patch` に絞る",
        "generated-test source-reference grounding が必要な場合だけ `read` を追加",
        "progress-projection saturation 後の requested-work recovery も `apply_patch` に絞り",
        "`provider_noncompliance_edit_recovery` や final-message hard edit recovery の `write` backstop",
        "malformed `apply_patch` が repeated no-side-effect",
        "malformed `apply_patch` を示す場合",
        "`malformed_apply_patch_write_recovery` は `apply_patch` と `write` の bounded hard recovery surface",
        "malformed `apply_patch` recovery の whole-file `write` backstop",
        "free-path `write(path, content)`",
        "`apply_patch_malformed_patch` は\r\n  `malformed_apply_patch_write_recovery` を開かない",
        "`apply_patch_malformed_patch` は\n  `malformed_apply_patch_write_recovery` を開かない",
        "current generated-test target の `apply_patch` authority",
        "existing `write` backstop",
        "artifact role 判定は `LanguageEvidenceAdapter` / `classify_artifact_target` を使い、Python `test_*.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_inactive_reminder_exact_edit_surface_authority = [
        "`apply_patch:test_*` が current action",
        "`write` 前提の next action",
        "`apply_patch` が current satisfying",
        "reminder は `apply_patch` / `patch_text` / active target path だけを action authority",
        "`write` wording は `write` が current edit primitive",
        "`write only the active target`",
        "`write` tool と `apply_patch` の admission",
        "shared `write_text_file` commit plan",
        "persist 前に `remove_file`",
        "`write` / `apply_patch` / future artifact-producing tools",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_truncated_output_exact_tool_surface_authority = [
        "Truncation follow-up guidance names registered typed surfaces such as `read` with `offset` / `limit` and `grep` with",
        "`path` set to the saved output file",
        "The guidance does not recommend unavailable `search` tool usage or shell command search as next-action authority",
        "The deterministic fixture checks registry-backed tool names, not only absence of one old wording",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_later_fixture_exact_edit_surface_authority = [
        "generic write/apply_patch feedback",
        "Invalid `apply_patch` write recovery uses canonical target identity",
        "reopen exact `write` recovery",
        "preserve an `apply_patch` call targeting",
        "Malformed write and non-edit invalid-argument fixtures are generic ToolLifecycle contracts",
        "`loop_impl_malformed_write_fixture_language_neutral`",
        "`preflight.tool_lifecycle.no_content_write_is_no_progress`",
        "TurnRuntime malformed-write recovery fixtures are workflow-neutral",
        "TurnRuntime malformed `apply_patch` recovery fixtures are workflow-neutral",
        "Malformed `apply_patch` recovery fixtures are generic ToolLifecycle contracts",
        "`loop_impl_malformed_apply_patch_fixture_language_neutral`",
        "TurnRuntime singleton write-argument repair fixtures are workflow-neutral",
        "TurnRuntime Singleton Write Argument Fixture Neutrality",
        "Singleton active-target write-argument repair fixtures are generic TurnRuntime contracts",
        "`repair_write_arguments_from_active_target`",
        "`loop_impl_singleton_write_argument_fixture_language_neutral`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_fr_summary_exact_read_write_surface_authority = [
        "can be read exactly once for patch\r\ngrounding",
        "can be read exactly once for patch\ngrounding",
        "non-target reads are rejected",
        "consumed after the exact target read",
        "Accepted Write Payload",
        "accepted write ToolCall / ToolOutput pairs",
        "accepted write target",
        "`preflight.prompt_replay.stale_write_arguments_summary_projection`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_repair_required_exact_edit_surface_authority = [
        "missing\r\n`write` / `apply_patch` from the provider-visible tool surface",
        "missing\n`write` / `apply_patch` from the provider-visible tool surface",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_generated_test_verification_exact_action_surface_authority = [
        "exact generated-test `write.path`",
        "source `write.path` を model-visible",
        "next action が `write` / `apply_patch`",
        "target-grounding `read`、`shell` / broad discovery 禁止",
        "`write` / `apply_patch` correction",
        "provider-visible surface は `write` / `apply_patch` / `todowrite`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_continuation_exact_action_string_authority = [
        "`write:<target>` を保持している場合",
        "singleton `write` へ畳む",
        "`tool_choice=required` を request / diagnostics / ToolRoute metadata",
        "singleton `shell:<command>` continuation",
        "shell schema の `properties.command.const`",
        "singleton `write:<target>` continuation",
        "`write` schema の `properties.path.const`",
        "実行前に `write` / `apply_patch` payload",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_mutation_recency_closeout_exact_surface_authority = [
        "`shell:<command>` continuation が残っていても",
        "既存の exact shell authority",
        "`ActionAuthority` が `shell:<command>` に縮退する",
        "write/apply_patch などの file-changing tool",
        "`read` / `list` / assistant prose",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_tool_language_specific_authority_drift = [
        "`write` / `apply_patch` の成功判定",
        "PromptBuilder は Python CLI が非 ASCII output",
        "`sys.stdout` / `sys.stderr` など",
        "docs/spec target への `write` / `apply_patch`",
        "active preflight `preflight.docs_spec.semantic_reconciliation_before_handoff`",
        "`sys.exit(2)`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_diagnostic_delta_verification_exact_authority_drift = [
        "Python stdlib、site-packages、runtime loader、threading/subprocess/unittest frame",
        "`sys.argv` などの dotted API symbol",
        "`write` は submitted content が post-change artifact",
        "一方 `apply_patch` は delta operation",
        "`subprocess.run(..., encoding=\"utf-8\")`",
        "Public command obligation extraction も Python command grammar",
        "Content-shape admission も Python/Text",
        "Python validator はこの",
        "`test_<module>.py` から",
        "`test_calculator.py` 固有",
        "broad `write` / `read` / `shell` surface",
        "provider-visible surface が\r\n`shell` に narrowed",
        "provider-visible surface が\n`shell` に narrowed",
        "`python -m unittest` 固有",
        "`test_<module>.py` / `<module>_test.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_stage_timeout_content_shape_exact_authority_drift = [
        "remaining verification command を exact shell authority",
        "tool lifecycle が `write` / `apply_patch` / shell mutation",
        "`C:\\workspace\\project\\src.py` と `C:/workspace/project/src.py`",
        "この contract は `calculator.py`",
        "Python / unittest / Windows に閉じた branch",
        "PowerShell / Bash / Python などの concrete adapter",
        "`subprocess.run(...)` を使う Python test artifact",
        "write/apply_patch candidate for test_<module>.py",
        "calculator / GUI / Python unittest",
        "次の item は exact verification rerun",
        "required verification command remains open as exact shell authority",
        "`test_*.py` / `*_test.py` のような generated-test target",
        "`def main()`、`input(...)`",
        "同じ target path への `write`",
        "write(path = test_<module>.py)",
        "`NO TESTS RAN` 後の reactive repair",
        "`write` / `apply_patch` の invalid arguments",
        "`apply_patch` context mismatch / expected-line failure",
        "bounded `write` replacement lane",
        "apply_patch/write rejected before side effect",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_short_form_fr_heading_authority =
        content.contains("## FR180 Failure Registry Projection Sequence Authority");
    let obsolete_comparison_surface = content.contains("OpenClaw");

    has_current_authority_line
        && has_current_state_contract_scope
        && has_current_owner_paths
        && !stale_current_build_product_authority
        && !stale_current_owner_path_authority
        && !stale_normative_body_product_authority
        && !stale_side_product_authority
        && !stale_unbackticked_body_product_authority
        && !stale_english_body_product_authority
        && !stale_widget_docs_authority
        && !stale_widget_test_artifact_authority
        && !stale_requested_work_specific_example_authority
        && !stale_active_target_repair_specific_example_authority
        && !stale_docs_route_specific_example_authority
        && !stale_docs_audit_specific_path_authority
        && !stale_verification_repair_language_case_authority
        && !stale_calculator_glob_authority
        && !stale_later_repair_verification_glob_language_authority
        && !stale_route_verification_environment_file_example_authority
        && !stale_public_output_subprocess_language_authority
        && !stale_content_shape_language_path_authority
        && !stale_docs_evidence_language_tool_path_authority
        && !stale_provider_supporting_tool_surface_authority
        && !stale_docs_surface_exact_tool_authority
        && !stale_recovery_grounding_exact_tool_authority
        && !stale_edit_surface_language_exact_authority
        && !stale_docs_grounding_shell_text_exact_surface
        && !stale_runner_repair_exact_surface_authority
        && !stale_inactive_reminder_exact_edit_surface_authority
        && !stale_truncated_output_exact_tool_surface_authority
        && !stale_later_fixture_exact_edit_surface_authority
        && !stale_fr_summary_exact_read_write_surface_authority
        && !stale_repair_required_exact_edit_surface_authority
        && !stale_generated_test_verification_exact_action_surface_authority
        && !stale_continuation_exact_action_string_authority
        && !stale_mutation_recency_closeout_exact_surface_authority
        && !stale_tool_language_specific_authority_drift
        && !stale_diagnostic_delta_verification_exact_authority_drift
        && !stale_stage_timeout_content_shape_exact_authority_drift
        && !stale_short_form_fr_heading_authority
        && !obsolete_comparison_surface
}

pub fn verification_harness_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("verification-harness.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let current_basis_section = content
        .split_once("主な根拠:")
        .and_then(|(_, rest)| rest.split_once("## 2.").map(|(section, _)| section))
        .unwrap_or("");
    let has_current_authority_line = content.contains(
        "本書は current build の `moyAI` における verification harness 実装の正本である。",
    );
    let has_current_owner_paths = [
        "`moyAI/src/tui/app.rs`",
        "`moyAI/src/desktop/app.rs`",
        "`moyAI/src/desktop/tauri_app.rs`",
        "`moyAI/src/desktop/web_model.rs`",
        "`moyAI/tests/*.rs`",
        "`moyAI/tests/support/harness.rs`",
        "`moyAI/tests/manual_ST/README.md`",
    ]
    .into_iter()
    .all(|owner_path| current_basis_section.contains(owner_path));
    let stale_current_build_product_authority = content.contains(
        "本書は current build の `moyai` における verification harness 実装の正本である。",
    );
    let stale_current_owner_path_authority =
        content.contains("`moyai/src/") || content.contains("`moyai/tests");
    let standard_environment_section = content
        .split_once("## 5. 標準 real-LLM 実行環境")
        .and_then(|(_, rest)| rest.split_once("## 6.").map(|(section, _)| section))
        .unwrap_or("");
    let has_current_standard_provider_profile = [
        "base URL: `http://127.0.0.1:1234`",
        "model: `qwen/qwen3.6-35b-a3b`",
        "provider metadata mode: `lm_studio_native_required`",
        "context window: `131072`",
        "max output tokens: `8192`",
    ]
    .into_iter()
    .all(|required| standard_environment_section.contains(required));
    let stale_standard_provider_profile = [
        "2026-04-20 時点の標準 real-LLM 検証先",
        "http://192.168.10.103:1234",
        "openai-compatible-fixture-model",
        "openai_compatible_only",
    ]
    .into_iter()
    .any(|stale| standard_environment_section.contains(stale));
    let stale_widget_fixture_authority = [
        "python -X utf8 widget.py 8 +",
        "generic `test_widget.py`",
        "exact `test_widget.py` repair",
        "exact `widget.py` repair",
        "generic `widget.py` / `test_widget.py`",
        "generic `test_widget.py` / `test_other.py`",
        "generic `docs/widget-design.md` target",
        "generic `tool.py` commands",
        "generic `widget.py`, `docs/widget-design.md`, and `test_widget.py`",
        "generic `tool.py` / `test_tool.py` evidence",
        "exact target `src/widget.py`",
        "sibling generated-test evidence `test_widget.py`",
        "supporting read of `test_widget.py`",
        "`src/widget.py` grounding",
        "no-progress read of `src/widget.py`",
        "verification target is `widget.py`",
        "obligations target `widget.py`",
        "required target is `widget.py`",
        "generated-test path `test_widget.py` remains evidence only",
        "`shell`, `list`, and `grep` remain excluded",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_exact_tool_surface_authority = [
        "successful read/list/search output",
        "current `todowrite` feedback pair",
        "RequiredAction::edit(Write, target)` を明示している一方で current executable surface が",
        "`apply_patch` だけの場合",
        "singleton target fallback で `apply_patch` action",
        "explicit `Write` action",
        "`apply_patch` を要求しているのに named `tool_choice=write`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_route_e2e_exact_tool_surface_authority = [
        "`write` は submitted content、`apply_patch` は workspace",
        "allowed tools は `write` のみ",
        "次 request が `read, write`",
        "request diagnostics が `tool_names=[\"write\"]`",
        "`write` only / `tool_choice=required` / `write.path=design.md`",
        "current active todo を保つだけの `todowrite`",
        "direct `read` 済みなら",
        "`read` / `shell` / broad discovery",
        "request diagnostics が `tool_names=[\"shell\"]`",
        "expected request diagnostics は `tool_names=[\"todowrite\"]`",
        "expected request diagnostics は `tool_names=[\"shell\"]`",
        "期待される current contract は `read` / `todowrite` / `write`",
        "期待される current contract は `write` / `apply_patch` / `todowrite`",
        "exact verification rerun の `shell` のみ",
        "大きい `write.content`",
        "`write:test_*.py`",
        "provider-visible surface が `shell` only",
        "stale `read` / `write`",
        "`allowed_tools=[shell]`",
        "`typed required action projection=shell:<command>`",
        "successful `write` / `apply_patch`",
        "`read` や追加 edit",
        "`shell:python -m unittest`",
        "exact `read`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_historical_route_language_artifact_authority = [
        "`python -X utf8 -m py_compile space_invader.py`",
        "`python -X utf8 -m unittest -v`",
        "ImportError, `cannot import name 'Game' from 'space_invader'`",
        "defined `Invader`, `Player`, `Bullet`",
        "`test_enemy_bullet_removed_when_off_screen`",
        "`NameError: name 'bullet' is not defined`",
        "the test creates `enemy_bullet` and then asserts against `bullet`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_late_source_generated_test_exact_surface_authority = [
        "keeps shell disallowed",
        "production-source-shaped `write` payloads to `test_*.py` / `*_test.py`",
        "case-specific unittest parser rule",
        "Invalid `write` / `apply_patch` arguments",
        "particular malformed\r\n`write` payload",
        "particular malformed\n`write` payload",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_midsection_language_command_output_authority = [
        "valid generated tests with `unittest`",
        "`Test*` classes, and `def test_...`",
        "subprocess stdin keyword usage for CLI tests and unittest lifecycle helpers",
        "placeholder command spans such as `python tool.py <mode> <input>`",
        "`self.assertIn(<expected>, result.stdout)`",
        "`self.assertIn(<expected>, result.stderr)`",
        "active docs work still required\r\n  `docs/calculator-design.md`",
        "active docs work still required\n  `docs/calculator-design.md`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_grounding_required_action_exact_surface_authority = [
        "Expected result: `read` remains available",
        "narrow the next surface to `apply_patch`,\r\n`write`, and `todowrite`",
        "narrow the next surface to `apply_patch`,\n`write`, and `todowrite`",
        "sibling generated-test reads, todo_write, source reads",
        "one requires `apply_patch` and the other requires `write`",
        "one `apply_patch:path` string",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_current_cli_or_case_artifact_authority = [
        "`moyai desktop --dir <workspace>`",
        "`python-unittest.log`",
        "case 固有の追加 log",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_retired_active_preflight_gate_authority = [
        "active preflight gate `preflight.lifecycle_kernel.provider_noncompliance_adjudication`",
        "`preflight.prompt_replay.tool_pair_symmetry`",
        "`preflight.prompt_replay.progress_projection_pair_omitted`",
        "`preflight.prompt_replay.stale_inactive_authoring_pair_omitted`",
        "`preflight.prompt_replay.compaction_orphan_assistant_repaired`",
        "`preflight.invariant.exact_active_authoring_write_surface`",
        "`preflight.tool_policy.generated_test_feedback_target_consistency`",
        "`preflight.tool_policy.generated_test_repair_focus_feedback_target_consistency`",
        "`preflight.turn_decision.contract_insufficient_blocks_source_write_authority`",
        "`preflight.turn_decision.post_edit_verification_rerun_authority`",
        "`preflight.tool_policy.shell_only_verification_pending_feedback`",
        "`preflight.tool_policy.post_edit_verification_rerun_feedback`",
        "`preflight.tool_policy.inactive_target_recovery_current_authority`",
        "`preflight.invariant.no_tests_ran_tool_route_repair_authority`",
        "active preflight gate `preflight.tool_lifecycle.verification_stable_tool_surface`",
        "active preflight gate `preflight.tool_lifecycle.progress_projection_stable_surface_guard`",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_language_runner_specific_current_harness_authority = [
        "generated Python test artifact",
        "`python -m unittest` が `NO TESTS RAN`",
        "`unittest.TestCase` / `def test_*`",
        "case3 Desktop GUI e2e は `python -m unittest`",
        "generated `test_calculator.py`",
        "generated unittest",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_unimplemented_gate_obligation_wording = [
        "The preflight suite must check repair supporting-context budget by failure class",
        "The preflight suite must include a source-owned public output stream mismatch fixture",
        "The harness must prove Codex-style repair and verification process ownership before live manual ST",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));

    has_current_authority_line
        && has_current_owner_paths
        && has_current_standard_provider_profile
        && !stale_current_build_product_authority
        && !stale_current_owner_path_authority
        && !stale_standard_provider_profile
        && !stale_widget_fixture_authority
        && !stale_exact_tool_surface_authority
        && !stale_route_e2e_exact_tool_surface_authority
        && !stale_historical_route_language_artifact_authority
        && !stale_late_source_generated_test_exact_surface_authority
        && !stale_midsection_language_command_output_authority
        && !stale_grounding_required_action_exact_surface_authority
        && !stale_current_cli_or_case_artifact_authority
        && !stale_retired_active_preflight_gate_authority
        && !stale_language_runner_specific_current_harness_authority
        && !stale_unimplemented_gate_obligation_wording
}

pub fn harness_engineering_roadmap_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("harness-engineering-roadmap.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_authority = content.contains("# Harness Engineering Roadmap")
        && content.contains("`moyAI`")
        && content.contains("Agent Harness Engine")
        && content.contains("typed lifecycle")
        && content.contains("active preflight gate-family")
        && content.contains("route taxonomy")
        && content.contains("workflow-neutral invariant roadmap")
        && content.contains("user-overridden model gate and fresh rerun boundary");
    let next_action_section = content
        .split_once("## 14. 直近の次アクション")
        .map(|(_, section)| section)
        .unwrap_or("");
    let stale_current_authority = [
        "`moyai`",
        "`moyai` を Agent Harness",
        "Case2 contract migration",
        "case2 mapping",
        "case2 typed contract",
        "case2 owner split",
        "case2 target",
        "Case2 は Phase G",
        "collision 専用 guard",
        "legacy adapter",
        "FR-080 through FR-098",
        "FR-099",
        "GameState.WON",
        "pending admission",
        "stored manual-ST adapter",
        "generated Python test artifact",
        "python-unittest.log",
        "py_compile log",
    ]
    .into_iter()
    .any(|stale| content.contains(stale));
    let stale_direct_next_actions = [
        "model availability gate を通す",
        "fresh representative sweep へ戻る",
        "fresh rerun で failure",
        "full library tests",
    ]
    .into_iter()
    .any(|stale| next_action_section.contains(stale));

    has_current_authority && !stale_current_authority && !stale_direct_next_actions
}

pub fn item_lifecycle_detail_current_authority_fixture_passes() -> bool {
    let docs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("docs")
                .join("design")
                .join("itemlifecycle-detail-design.md")
        });
    let Some(docs_path) = docs_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(docs_path) else {
        return false;
    };

    let has_current_target_path =
        content.contains("This document defines the target item lifecycle for `moyAI/`.");
    let has_current_design_target = content.contains(
        "model and translates it into the `moyAI` runtime / protocol / harness target design.",
    );
    let current_authority_intro = content
        .split_once("## 1.0 Current Authority Notice")
        .and_then(|(_, rest)| {
            rest.split_once("Current design rules:")
                .map(|(section, _)| section)
        })
        .unwrap_or("");
    let has_current_authority_notice = [
        "current `moyAI` single control-plane item lifecycle",
        "Thread / Turn / Item protocol",
        "TurnControlEnvelope",
        "ActionAuthority",
        "ProjectionBundle",
        "ToolLifecycleEnvelope",
        "runtime capability hydration",
        "event-sourced runtime",
        "route-owned obligations",
        "app boundary projection",
        "memory / compaction continuity",
        "active preflight gate families",
        "historical FR evidence boundary",
    ]
    .into_iter()
    .all(|required| current_authority_intro.contains(required));
    let stale_current_authority_intro = [
        "As of the 2026-05-26 Codex alignment implementation pass",
        "TurnLifecyclePlan owns dispatch",
        "TurnLifecycleKernel owns provider edit surface narrowing",
        "`PromptBuilder` must not own provider surface policy",
        "`TurnRuntime` must not reintroduce prompt-module compatibility wrappers",
    ]
    .into_iter()
    .any(|stale_phrase| current_authority_intro.contains(stale_phrase));
    let stale_design_rule_lifecycle_plan_owner = [
        "`TurnLifecyclePlan`, `ActionAuthority`, `ProjectionBundle`, and `TurnControlEnvelope`",
        "compiled into the same\r\n  `TurnLifecyclePlan`",
        "compiled into the same\n  `TurnLifecyclePlan`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let lifecycle_owner_section = content
        .split_once("## 1.1 ")
        .and_then(|(_, rest)| rest.split_once("### 1.1.1").map(|(section, _)| section))
        .unwrap_or("");
    let has_current_lifecycle_owner_section = [
        "Current Single Control-Plane Item Lifecycle Owner Map",
        "TurnControlEnvelope",
        "ActionAuthority",
        "ToolLifecycleEnvelope",
        "ProjectionBundle",
        "event-sourced runtime",
        "route-owned obligations",
    ]
    .into_iter()
    .all(|required| lifecycle_owner_section.contains(required));
    let stale_lifecycle_kernel_section = [
        "2026-05-25 Codex-Aligned Lifecycle Kernel Redesign",
        "`TurnLifecycleKernel` | Own the turn-level decision graph",
        "agent loop plus many local guards",
        "lifecycle kernel",
        "bypass the kernel",
    ]
    .into_iter()
    .any(|stale_phrase| lifecycle_owner_section.contains(stale_phrase));
    let stale_lifecycle_kernel_global_phrase = content.contains("bypass the kernel");
    let stale_required_tool_choice_kernel_boundary = [
        "the lifecycle kernel records it as provider",
        "lifecycle kernel / provider\r\nadapter boundary",
        "lifecycle kernel / provider\nadapter boundary",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_source_artifact_admission_payload_authority =
        content.contains("source artifact target must receive executable source payload");
    let stale_provider_noncompliance_owner_authority = [
        "Superseded By Lifecycle-Kernel Provider Noncompliance",
        "`ActionAdjudicator` reject it as",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let runtime_flow_section = content
        .split_once("### 4.2 Current Runtime Flow")
        .and_then(|(_, rest)| rest.split_once("### 4.3").map(|(section, _)| section))
        .unwrap_or("");
    let stale_runtime_flow_compatibility_bridge_authority = [
        "converts legacy `RunEvent` values",
        "current bridge between the older session model",
        "assistant-start compatibility message",
        "compatibility row whenever the event has a session/message side effect",
        "compatibility transcript projection",
        "`compatibility_transcript` is an explicit storage-local projection for legacy display/test",
        "transcript fallback",
    ]
    .into_iter()
    .any(|stale_phrase| runtime_flow_section.contains(stale_phrase));
    let prompt_replay_section = content
        .split_once("### 4.3 Current Prompt Replay Boundary")
        .and_then(|(_, rest)| rest.split_once("### 4.4").map(|(section, _)| section))
        .unwrap_or("");
    let tool_lifecycle_section = content
        .split_once("### 4.4 Current Tool Lifecycle")
        .and_then(|(_, rest)| rest.split_once("### 4.5").map(|(section, _)| section))
        .unwrap_or("");
    let required_replay_section = content
        .split_once("### 5.1 Single Source Of Provider Replay")
        .and_then(|(_, rest)| rest.split_once("### 5.2").map(|(section, _)| section))
        .unwrap_or("");
    let stale_transcript_compatibility_authority = [
        "Transcript` remains a compatibility projection",
        "appends a compatibility",
        "MessagePart::ToolCall",
        "MessagePart::ToolResult",
        "RunEvent::ToolCallPending",
        "RunEvent::ToolCallCompleted",
        "RunEvent::ToolCallFailed",
        "legacy import",
        "compatibility tests",
    ]
    .into_iter()
    .any(|stale_phrase| {
        prompt_replay_section.contains(stale_phrase)
            || tool_lifecycle_section.contains(stale_phrase)
            || required_replay_section.contains(stale_phrase)
    });
    let provider_replay_mapping_section = content
        .split_once("### 5.2 Canonical HistoryItem To Provider Message Mapping")
        .and_then(|(_, rest)| rest.split_once("### 5.3").map(|(section, _)| section))
        .unwrap_or("");
    let closeout_classification_section = content
        .split_once("### 5.6 Closeout Classification Contract")
        .and_then(|(_, rest)| rest.split_once("### 5.7").map(|(section, _)| section))
        .unwrap_or("");
    let stale_provider_replay_closeout_example_authority = [
        "compatibility import only",
        "requested generated-test artifact",
        "source artifact written, generated-test artifact missing",
    ]
    .into_iter()
    .any(|stale_phrase| {
        provider_replay_mapping_section.contains(stale_phrase)
            || closeout_classification_section.contains(stale_phrase)
    });
    let ui_transcript_boundary_section = content
        .split_once("### 5.9 UI / Markdown / Transcript Boundary")
        .and_then(|(_, rest)| rest.split_once("### 5.10").map(|(section, _)| section))
        .unwrap_or("");
    let implementation_slices_section = content
        .split_once("### 5.11 Implementation Slices")
        .and_then(|(_, rest)| rest.split_once("## 6.").map(|(section, _)| section))
        .unwrap_or("");
    let implemented_fr065_section = content
        .split_once("## 8. Implemented Design Consequence For FR03-2026-05-07-065")
        .and_then(|(_, rest)| rest.split_once("## 9.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_ui_transcript_compatibility_boundary_authority = [
        "old CLI",
        "TUI compatibility view",
        "legacy import review",
        "display/export/legacy import modules",
        "compatibility\r\ntranscript projection",
        "compatibility\ntranscript projection",
    ]
    .into_iter()
    .any(|stale_phrase| {
        ui_transcript_boundary_section.contains(stale_phrase)
            || implementation_slices_section.contains(stale_phrase)
            || implemented_fr065_section.contains(stale_phrase)
    });
    let immediate_consequence_section = content
        .split_once("## 9. Immediate Design Consequence For FR03-2026-05-08-066")
        .and_then(|(_, rest)| {
            rest.split_once("Implementation consequence:")
                .map(|(section, _)| section)
        })
        .unwrap_or("");
    let stale_immediate_consequence_exact_surface_authority = [
        "one requested artifact was written",
        "another requested artifact remained open",
        "verification found no tests",
        "runtime `SessionCompleted`",
        "`calculator.py`",
        "`test_calculator.py`",
        "`process_phase=discover`",
        "`active_targets=[test_calculator.py]`",
        "`python -m unittest`",
        "`tool_choice=auto`",
        "`write:<target>` / `shell:<command>`",
        "tool surface + tool_choice required",
        "representative vision route",
        "generated-test\r\nartifact roles",
        "generated-test\nartifact roles",
        "`turn step budget reached before completion`",
        "`update_plan`-like",
    ]
    .into_iter()
    .any(|stale_phrase| immediate_consequence_section.contains(stale_phrase));
    let implementation_consequence_owner_surface_section = content
        .split_once("## 15. Immediate Design Consequence For FR03-2026-05-08-073")
        .and_then(|(_, rest)| rest.split_once("## 18.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_implementation_consequence_owner_surface_authority = content
        .contains("`TurnRuntime` maintains a progress-projection no-progress counter")
        || [
            "document / generated-test artifact targets",
            "post-fix representative vision route",
            "scenario-contract\r\nreference artifacts",
            "scenario-contract\nreference artifacts",
            "documentation, source, and generated-test deliverable roles",
            "`update_plan` is",
            "`typed required action projection`",
            "support / enumeration / search-family output",
            "plan-update side-channel output",
            "Codex `update_plan`",
            "`TurnLifecycleKernel` must validate",
            "`ToolLifecycleRuntime::route_adjudicated_call()`",
            "`TurnRuntime` keeps the stable registry",
            "`path` / `content` arguments",
            "manual ST case, game name, artifact name, or provider\r\n  wording",
            "manual ST case, game name, artifact name, or provider\n  wording",
            "`completion.closeout_ready=true`",
            "`completion.verification_pending=false`",
        ]
        .into_iter()
        .any(|stale_phrase| {
            implementation_consequence_owner_surface_section.contains(stale_phrase)
        });
    let route_target_reference_section = content
        .split_once("## 18. Immediate Design Consequence For FR03-2026-05-08-076")
        .and_then(|(_, rest)| rest.split_once("### 19.3").map(|(section, _)| section))
        .unwrap_or("");
    let stale_route_target_reference_exact_surface_authority = [
        "`reduce_session_state_from_history_items()`",
        "Required Vision Route B",
        "`TurnRuntime` validates submitted verification",
        "documentation and\r\ngenerated-test deliverable targets",
        "documentation and\ngenerated-test deliverable targets",
        "initial user + image + scenario contract request",
        "scenario contract artifacts",
        "process_phase=Verify",
        "Japanese GUI UAT prompt",
        "`。hello.py`",
        "`Pythonで小さな挨拶CLIを作ってください。hello.py`",
        "`write(path=\"hello.py\")`",
        "source and generated-test\r\n   artifact tokens",
        "source and generated-test\n   artifact tokens",
        "GUI continuation",
        "project guide artifact",
        "Existing code / test artifacts mentioned as documentation subjects by phrases like",
        "`<target> の使い方`",
        "`<target> を更新`",
    ]
    .into_iter()
    .any(|stale_phrase| route_target_reference_section.contains(stale_phrase));
    let answer_replay_verification_surface_section = content
        .split_once("### 19.3 FR10-2026-05-18-020 Answer-Only Vision Turn Final Message")
        .and_then(|(_, rest)| rest.split_once("## 23.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_answer_replay_verification_surface_authority = [
        "Desktop GUI attached",
        "Japanese description of visible game elements",
        "`closeout_ready=false`",
        "`ProcessPhase::Discover`",
        "`write`, `apply_patch`, or verification",
        "case2a or vision-specific fixture",
        "representative\r\n  vision route",
        "representative\n  vision route",
        "documentation, scenario-contract, and source artifacts",
        "generated\r\n  test artifact",
        "generated\n  test artifact",
        "`000030_state_snapshot_recorded.json`",
        "`000043_turn_control_envelope.json`",
        "`000046_tool_executed.json`",
        "`000047_run_terminalized.json`",
        "source-artifact edit",
        "generated-test deliverable",
        "prior completed source file",
        "current active test/document/deliverable\r\n  target",
        "current active test/document/deliverable\n  target",
        "documentation filenames",
        "generated\r\n  test filenames",
        "generated\n  test filenames",
        "`workspace_diff_manifest.json`",
        "`process_phase=verify`",
        "`completion.verification_pending=true`",
        "`completion.closeout_ready=false`",
        "`active_targets=[]`",
        "`000048_tool_executed.json`",
        "`000053_run_terminalized.json`",
        "`operation_bound_effective_tool_names`",
        "single\r\n  command-execution tool",
        "single\n  command-execution tool",
        "Codex `built_tools()`",
        "`ToolRouter::from_config()`",
        "representative vision route created\r\nscenario-contract and source",
        "representative vision route created\nscenario-contract and source",
        "documentation and generated-test deliverables",
        "source-artifact edit actions",
    ]
    .into_iter()
    .any(|stale_phrase| answer_replay_verification_surface_section.contains(stale_phrase));
    let feedback_toolchoice_continuation_surface_section = content
        .split_once("## 23. FR10-2026-05-08-004 Result-Scoped No-Progress Guard Gap")
        .and_then(|(_, rest)| rest.split_once("## 30.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_feedback_toolchoice_continuation_surface_authority = [
        "representative vision route",
        "scenario-contract artifacts",
        "documentation, source, and generated-test deliverables",
        "`000023_run_terminalized.json`",
        "targets were still documentation, source, and generated-test deliverables",
        "`operation_non_content_no_progress_key`",
        "broad authoring state,\r\nallowed surface, and tool choice",
        "broad authoring state,\nallowed surface, and tool choice",
        "representative vision route again stopped",
        "`0d04a5...`, `26b73c...`, and `04a104...`",
        "`progress_projection_no_progress_key`",
        "`ToolLifecycleRuntime::complete_executed_call`",
        "`completion_metadata`",
        "Raw progress\r\nside-channel `ToolResult.metadata`",
        "Raw progress\nside-channel `ToolResult.metadata`",
        "A representative\r\nvision route again produced only scenario-contract artifacts",
        "A representative\nvision route again produced only scenario-contract artifacts",
        "provider-visible output text for the progress side-channel",
        "provider-visible output text for the grounding action",
        "open requested-work targets remained documentation, source, and generated-test artifacts",
        "provider-visible `ToolResult` text",
        "tool_feedback_envelope",
        "A representative vision route created documentation, scenario-contract, and\r\nsource artifacts",
        "A representative vision route created documentation, scenario-contract, and\nsource artifacts",
        "`000058_state_snapshot_recorded.json`",
        "`000059_turn_control_envelope.json`",
        "`000070_tool_executed.json`",
        "`000071_run_terminalized.json`",
        "[tool feedback]",
        "`write:<target>` grammar",
        "typed ToolResult output feedback",
        "`000094_tool_executed.json`",
        "documentation/source/generated-test active targets",
        "`000005_turn_control_envelope.json`",
        "`tool_result_feedback` surface footer",
        "Codex `update_plan`",
        "`Plan updated`",
        "owned by `ToolRouter`",
        "provider request still forced a required tool-choice policy",
        "required provider tool-choice policy",
        "`000126_tool_executed.json`",
        "`000127_run_terminalized.json`",
        "provider tool_choice=required",
        "todowrite / progress projection",
        "Codex standard Responses requests use automatic provider tool-choice policy",
        "`PlanHandler` returns successful `FunctionCallOutput(\"Plan updated\")`",
        "provider-side required tool choice",
        "preflight.turn_decision.codex_stable_tool_surface_authority",
        "A representative vision route completed the runtime turn",
        "missing documentation and generated-test artifacts",
        "failed verification command evidence",
        "TurnRuntime:",
        "emit SessionCompleted",
        "Manual ST / app closeout hook:",
        "session_id, continue_last=false",
        "typed required action projection",
    ]
    .into_iter()
    .any(|stale_phrase| {
        feedback_toolchoice_continuation_surface_section.contains(stale_phrase)
    });
    let continuation_provider_replay_surface_section = content
        .split_once("Verification evidence contract:")
        .and_then(|(_, rest)| rest.split_once("## 37.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_continuation_provider_replay_surface_authority = [
        "verification_command_log:",
        "missing generated-test artifact",
        "manual ST route execution",
        "initial manual ST user turn",
        "typed image attachments",
        "same session_id",
        "continue_last=false",
        "ManualStCloseoutEvidence",
        "build_hook_prompt_message",
        "role=\"user\"",
        "typed required action projection",
        "preflight.closeout.verification_repair_continuation_hook",
        "provider request fields",
        "provider request had accumulated repeated image attachments",
        "original vision\r\ninput was reattached",
        "original vision\ninput was reattached",
        "model-visible prompt text naming the local image filename",
        "document/image conversion request",
        "repeated workspace image enumeration",
        "User image attachment:",
        "source_path only as diagnostic",
        "input_image data URL / base64 part",
        "manual ST task text",
        "`UserInput::LocalImage`",
        "`ContentItem::InputImage`",
        "`UserMessageEvent.local_images`",
        "`view_image` tool lifecycle",
        "`content_parts_to_user_message`",
        "representative route no longer failed on image rediscovery",
        "artifact-role repair target remained active",
        "one per-stage continuation counter",
        "generated-test failure labels",
        "author generated-test-suite.class.method",
        "process_phase=closeout",
        "active_work_kind=requested_work_authoring",
        "provider_surface=empty",
        "repair_targets=[artifact-role:source]",
        "suite.class.method",
        "preflight.closeout.verification_labels_not_requested_work",
        "run agent error: provider request would omit the active user query before dispatch",
        "Stop-hook prompts are recorded as new `role=user` messages",
        "trailing `Compaction` item",
        "`CompactionContinuity`",
        "preflight.prompt_replay.active_user_hook_non_droppable",
        "No user query found in messages",
        "orphan `tool` role messages",
        "tool-output(call_id=historical-support-call)",
        "\"tool_role\": \"supporting_context_action\"",
        "\"model_arguments\": { \"target\": \"artifact-role:source\" }",
        "effective_arguments` or compatibility `arguments`",
        "`ToolOutput`",
        "`FunctionCall` and `FunctionCallOutput`",
        "Provider replay tool pair policy:",
        "effective_arguments",
        "adjusted_arguments",
        "model_arguments",
        "preflight.prompt_replay.tool_pair_symmetry",
        "generated-test artifact role",
        "\"[omitted inactive authoring target]\"",
        "\"[omitted stale inactive authoring payload; current active requested-work targets are artifact-role:generated-test]\"",
        "AssistantToolCalls(content-changing-edit",
    ]
    .into_iter()
    .any(|stale_phrase| continuation_provider_replay_surface_section.contains(stale_phrase));
    let progress_projection_tool_surface_section = content
        .split_once("## 37. FR10-2026-05-08-018 Historical Progress Projection Is Not Current Authoring Authority")
        .and_then(|(_, rest)| rest.split_once("## 44.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_progress_projection_tool_surface_authority = [
        "workflow-neutral documentation and generated-test artifact roles",
        "previously active source artifact",
        "stale source artifact",
        "Plan/progress projection item:",
        "historical progress side-channel assistant tool-call JSON",
        "Codex `update_plan`",
        "provider-visible assistant `todowrite` tool-call JSON",
        "provider-visible `tool` output",
        "preflight.prompt_replay.progress_projection_pair_omitted",
        "`progress_projection_payload_omitted`",
        "RequestedWorkAuthoring open:",
        "`PlanHandler` records a `PlanUpdate` event",
        "`FunctionCallOutput(\"Plan updated\")`",
        "preflight.tool_lifecycle.authoring_progress_projection_saturates",
        "source artifact, correctly rotated active requested-work targets to documentation and\r\ngenerated-test artifact roles",
        "source artifact, correctly rotated active requested-work targets to documentation and\ngenerated-test artifact roles",
        "stale source artifact",
        "`stale_inactive_authoring_payload_omitted`",
        "call-id-scoped failure evidence",
        "broad whole-artifact edit arguments",
        "WrongAuthoringTarget content-changing edit call:",
        "`FunctionCallOutput` scoped to the originating `call_id`",
        "moyAI-specific weak surface",
        "fresh failed writes",
        "`tool_call_id`",
        "wrong-target\r\n  ToolResult feedback",
        "wrong-target\n  ToolResult feedback",
        "preflight.tool_lifecycle.edit_surface_registry_symmetry",
        "preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard",
        "provider-visible `tool_names`",
        "tool_exists=true, tool_allowed=false",
        "corrective ToolResult",
        "known tool",
        "FR10-020's core edit removal",
        "whole-artifact and patch-oriented edit actions",
        "including default provider-dispatch / broad-surface contexts",
        "supporting_context ToolOutput",
        "operation-bound effective tools remove progress projection",
        "Codex `update_plan` is a normal call-id-scoped function lifecycle item",
        "preflight.tool_lifecycle.progress_projection_stable_surface_guard",
        "Codex `PlanHandler` explicitly treats plan inputs",
        "Required action",
        "post-FR10-023 route created a source artifact",
        "documentation and generated-test artifact roles",
        "`progress_projection_payload_omitted`",
        "call-id-scoped ToolOutput feedback",
        "content-changing edit(source artifact)",
        "active targets rotate to documentation and generated-test artifact roles",
        "ToolOutput says progress_projection/no_progress",
        "FunctionCallOutput",
        "assistant ToolCall + ToolOutput pair",
        "orphan ToolOutput",
    ]
    .into_iter()
    .any(|stale_phrase| progress_projection_tool_surface_section.contains(stale_phrase));
    let repair_target_evidence_surface_section = content
        .split_once(
            "## 44. FR10-2026-05-08-025 Failed Wrong-Target Output Must Stay Call-Id-Scoped",
        )
        .and_then(|(_, rest)| rest.split_once("## 47.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_repair_target_evidence_surface_authority = [
        "content-changing edit(source artifact)",
        "documentation and generated-test artifact roles",
        "failed_inactive_authoring_failure_scoped_summary",
        "failed ToolCall/ToolOutput pair",
        "stale source edit",
        "provider ToolCall/ToolOutput pair",
        "AssistantToolCall with the submitted arguments",
        "ToolOutput with wrong-target failure text",
        "canonical ToolCall/ToolOutput event history",
        "Codex `FunctionToolOutput::to_response_item()`",
        "`FunctionCallOutput { call_id, output }`",
        "`FunctionCallOutput` with `success: Some(false)`",
        "preflight.tool_lifecycle.edit_surface_registry_symmetry",
        "failed_inactive_executable_pair_omitted",
        "failed_inactive_non_executable_feedback_projected",
        "successful_stale_inactive_payload_summary_only",
        "source-artifact edit",
        "generated-test artifact role",
        "operation exact\r\ntarget",
        "operation exact\ntarget",
        "public_state_assertion_mismatch",
        "generated-test refs",
        "active_targets = artifact-role:generated-test",
        "repair_owner = source",
        "exact_target = artifact-role:generated-test",
        "source_owned_repair_generated_test_target",
        "latest_typed_verification_failure_context()",
        "`FileChange` targets",
        "`PublicStateAssertionMismatch`",
        "`RepairOperationTemplate`",
        "preflight.state_reducer.verification_failure_preserves_repair_target_authority",
        "source_owned_recent_file_change_target_preserved",
        "behavior assertion prose",
        "source_refs contain behavior assertion prose",
        "active_targets include behavior prose and generated-test artifact role",
        "source-owned operation exact target falls back to generated-test artifact role",
        "provider transport\r\n  -> later SSE decode error",
        "provider transport\n  -> later SSE decode error",
        "generated-test artifact-role refs",
        "provider SSE / response-body decode errors",
        "verification_failure_repair_targets()",
        "first_non_test_target()",
        "first_mutable_source_target()",
        "preflight.route_evidence.schema",
    ]
    .into_iter()
    .any(|stale_phrase| repair_target_evidence_surface_section.contains(stale_phrase));
    let stale_target_path =
        content.contains("This document defines the target item lifecycle for `moyai/`.");
    let stale_design_target = content.contains(
        "model and translates it into the `moyai` runtime / protocol / harness target design.",
    );
    let stale_domain_fixture_authority = [
        "These fixtures use `widget.py` / `test_widget.py`",
        "`component.py` / `test_component.py` plus a scenario contract reference block",
        "using generic `widget.py` / `test_widget.py` style fixtures",
        "uses `widget.py` / `test_widget.py` content-shape mapping as a generic",
        "fixture uses generic `widget.py` / `test_widget.py` mapping",
        "regression fixture uses `test_widget.py` and generic production markers",
        "preflight fixture uses generic `widget.py` / `test_widget.py` with semantic output refs",
        "The lower-tier gates use generic `widget.py` / `test_widget.py` repair evidence",
        "Add a lower-tier fixture with generic `widget.py` / `test_widget.py` names",
        "Add lower-tier fixture with generic `widget.py`, `docs/widget-design.md`, and `test_widget.py`",
        "using generic `widget.py`, `test_widget.py`, and docs artifact inventory",
        "preflight fixture proves a generated-test syntax error remains `test_widget.py`",
        "`python -m unittest test_widget -v` and the same command",
        "`widget.py` / `test_widget.py` fixture for post-repair",
        "projects `widget.py`, not",
        "target `test_arcade_game.py`",
        "such as `docs/calculator-design.md`",
        "generic `component.py` / `test_component.py` evidence",
        "`component.py` exact edit repair shape",
        "keep the fixture generic (`component.py` / `test_component.py`)",
        "evidence using generic `component.py` / `test_component.py` fixture names",
        "`test_component.py` active targets and prove",
        "singleton create-target authority using generic `component.py` / `test_component.py` names",
        "lower-tier fixture uses `component.py` /",
        "The fixture uses `component.py`, `test_component.py`, and `docs/component-design.md`",
        "Keep the fixture generic: `component.py`, `test_component.py`, and",
        "`docs/component-design.md` and protected `scenario_contract.md/json` references",
        "state reducer and turn decision gate families. The fixture uses `widget.py` / `test_widget.py`",
        "generic deterministic fixture using `widget.py` / `test_widget.py`",
        "using `source.py` / `test_source.py`",
        "state reducer / preflight fixture using `source.py` and `test_source.py`",
        "top-level `def` / `class` / `import` lines",
        "The fixture is generic over `component.py` / `test_component.py`",
        "The fixture is generic over `widget.py` / `test_widget.py`",
        "using generic `tool.py` / `test_tool.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_implementation_status_future_action = [
        "Full deterministic verification, active preflight, LM Studio model gate, and fresh rerun follow.",
        "Full deterministic verification,\r\nactive preflight, LM Studio model gate, and fresh rerun are next.",
        "Full deterministic verification,\nactive preflight, LM Studio model gate, and fresh rerun are next.",
        "Full verification, active preflight, LM Studio model gate, and fresh rerun are next.",
        "は green。full deterministic verification、active preflight、",
        "LM Studio model gate、fresh rerun は次に実施する。",
        "green。full deterministic verification、active preflight、LM Studio model gate、fresh rerun は次に実施する。",
        "は green。fresh rerun は次に実施する。",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_implementation_status_registered_before_fix =
        content.contains("Implementation status: registered before fix");
    let stale_short_current_fr_heading = content.contains("## FR180 Registry Projection Lifecycle");
    let stale_current_product_authority = content.contains("`moyai` treats artifact admission")
        || content.contains("`moyai`\r\ntherefore treats")
        || content.contains("`moyai`\ntherefore treats");
    let stale_current_runtime_product_path_authority = [
        "The `moyai` design consequence",
        "## 4. moyai Current Item Lifecycle",
        "`moyai` already has protocol item vocabulary",
        "`moyai` now has",
        "`moyai` control-plane",
        "equivalent `moyai` invariant",
        "depending on the child process. `moyai`",
        "## 5. Required moyai Target Design",
        "`moyai` must keep Codex's final answer separation",
        "`moyai/src/protocol/`",
        "`moyai/src/agent/loop_impl.rs`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_closeout_route_layer_authority = [
        "open_obligation = requested artifact test_calculator.py",
        "evidence = calculator.py written, test_calculator.py missing, unittest found no tests",
        "Codex-style item lifecycle and `moyai` route evaluation",
        "## 6. Codex / moyai Design Differences",
        "`moyai` may keep some additional typed lifecycle concepts",
        "The additional `moyai` route layer",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_consequence_product_authority = [
        "`moyai` allowed provider input to be built through a compatibility",
        "keeps `moyai`'s typed",
        "wording while `README.md` / `test_space_invader.py` or any equivalent open artifact targets stay",
        "In `moyai`, that extra harness contract must stay below the item lifecycle",
        "Therefore `moyai` must validate active-target membership at the tool lifecycle boundary",
        "may contain `hello.py` and `test_hello.py`",
        "Documentation output targets such as `README.md` remain deliverable authoring targets",
        "Therefore `moyai` must keep tool availability stable and move requested-work satisfaction",
        "`moyai` local-LLM hardening can add terminal guards",
        "`moyai` can add app-level operation classification",
        "`moyai`'s requested-work target set is an added app-level contract",
        "moyai-specific weak",
        "Therefore `moyai` must express docs-only obligations",
        "through moyai's Windows shell bootstrap",
        "the moyai shell environment",
        "moyai may normalize its shell",
        "`moyai/src/agent/docs_semantic_contract.rs`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_tooloutput_metadata_authority = [
        "observed through FileChange or ToolOutput changed_files metadata",
        "ToolOutput metadata that carries `changes` / `changed_files` is treated as canonical content-change",
        "canonical FileChange / ToolOutput metadata",
        "FileChange / ToolOutput metadata",
        "or ToolOutput metadata that records a successful changed file",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_current_authority_exact_edit_surface_recovery = [
        "Malformed `apply_patch` recovery is exact current-item authority",
        "next dispatch narrows to named `write:<target>`",
        "used for other exact writes",
        "Malformed `apply_patch` recovery compares submitted patch targets",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_prompt_verification_repair_exact_shell_surface =
        content.contains("`shell:verify-workflow --docs`");
    let stale_current_edit_operation_exact_surface = [
        "When the active action is `apply_patch`",
        "Validation fails if `read`, `todowrite`, `shell`, or an alternate edit tool remains executable",
        "treats `write:<target>` as satisfied by an `apply_patch`-only provider surface",
        "generic `write or apply_patch` wording",
        "When a write/apply_patch payload was rejected",
        "`apply_patch` FileChange admission is operation-derived",
        "raw write/apply_patch callers must not add a second",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_tool_lifecycle_runtime_exact_surface_command = [
        "including `shell`, becomes current artifact progress",
        "contract used by `write` and `apply_patch`",
        "must not suggest `write`, `apply_patch`, or a path-specific next edit",
        "A `write` or\r\n  `apply_patch` FunctionCallOutput",
        "A `write` or\n  `apply_patch` FunctionCallOutput",
        "The\r\n  `apply_patch` path must detect",
        "The\n  `apply_patch` path must detect",
        "same side-effect-free no-progress ToolOutput as `write`",
        "`python -X utf8 -m py_compile target.py` satisfy the same obligation as",
        "`python -m py_compile target.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_lifecycle_kernel_redesign_exact_surface = [
        "`write` and `apply_patch` raw ToolResults must both",
        "no-content write/apply_patch feedback",
        "edit tools such as `write` / `apply_patch`",
        "stale `shell`\r\n  verification reruns",
        "stale `shell`\n  verification reruns",
        "edit tools such as\r\n  `apply_patch` / `write`",
        "edit tools such as\n  `apply_patch` / `write`",
        "provider-portable named `write` operation",
        "normal broad `write`, and contains `write` only",
        "compiled `ToolChoice` is `Named(Write)`",
        "model submits `write` / `apply_patch` for a different target",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_shared_vocabulary_fr10_exact_surface = [
        "If `shell` or any\r\nother file-mutating tool creates",
        "If `shell` or any\nother file-mutating tool creates",
        "A Python source target must receive executable source payload",
        "unittest / pytest content",
        "Shell ToolOutputs are complete only after process lifecycle closure",
        "When a `write`\r\nFunctionCall is rejected",
        "When a `write`\nFunctionCall is rejected",
        "smaller valid `write` or a concise\r\n`apply_patch`",
        "smaller valid `write` or a concise\n`apply_patch`",
        "`tool_choice=required` rather than `named:write`",
        "keeps only `write` / `apply_patch` executable with `tool_choice=required`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_current_tool_lifecycle_single_ledger_exact_surface = [
        "The file mutation authority is single-ledger. `write` and `apply_patch` record",
        "`shell` may still be available for\r\nverification and targeted diagnostics",
        "`shell` may still be available for\nverification and targeted diagnostics",
        "A shell-side\r\nrewrite must not leave",
        "A shell-side\nrewrite must not leave",
        "the next canonical\r\n`write` or `apply_patch` is blocked",
        "the next canonical\n`write` or `apply_patch` is blocked",
        "therefore forces Python child processes toward UTF-8",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_closeout_contract_exact_surface_remaining_slices = [
        "tool lifecycle evidence for writes, patches, shell verification",
        "Remaining FR03-066 slices:",
        "Add `RuntimeCompleted` / `CloseoutClass` / `RouteVerdict` typing",
        "Add lower-tier tests for final assistant with open required artifact",
        "Rerun model availability gate, active preflight, and representative route fresh rerun",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_operation_intent_exact_surface_domain_fixture = [
        "verification active work -> shell tool call",
        "Required Vision Route B full stopped in case2a",
        "content-changing write/apply_patch output",
        "read/list/search -> supporting_context output",
        "todo_write -> progress_projection output",
        "write/apply_patch with file evidence",
        "submitted write/apply_patch with file evidence",
        "submitted read/inspect/todowrite outside satisfying class",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_verification_route_map_exact_surface_domain = [
        "After `python -m py_compile space_invader.py` passed",
        "`apply_patch` / `write` as the effective operation surface",
        "provider repeatedly wrote\r\n`space_invader.py`",
        "provider repeatedly wrote\n`space_invader.py`",
        "effective surface = write/apply_patch after supporting context exists",
        "Codex keeps `write` / file-edit schemas stable",
        "A write / patch to an inactive, already-satisfied, or unrelated target",
        "Add pre-execution target-membership validation for submitted `write` calls",
        "Space Invader / README / test-file branch",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_fr10_route_map_exact_surface_domain = [
        "how to use `hello.py`, and run `python -m unittest`",
        "prior write(space_invader.py) call/output remains intact",
        "wrong write(space_invader.py) call/output remains intact",
        "large previous `write` content",
        "Required Vision Route B case2a failed quickly",
        "allowed tools as `apply_patch` / `write`",
        "required commands `python -m py_compile space_invader.py` / `python -m unittest`",
        "allowed tools `[\"shell\"]`",
        "`typed required action projection=shell:<command>`",
        "provider-visible `allowed_tools=[shell]`",
        "running `python -m unittest`",
        "with allowed tools `[\"apply_patch\",\"write\"]`",
        "submitted read/list/search/todowrite",
        "submitted write/apply_patch",
        "`read(scenario_contract.md)` classified as `supporting_context`",
        "`inspect_directory(\".\")` classified as `supporting_context`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_fr10_feedback_target_projection_exact_surface_domain = [
        "Tool `todowrite` returned progress projection without artifact or workspace progress 3 time(s)",
        "`read(scenario_contract.md)` executed as normal supporting context",
        "three `todowrite` calls executed as progress projections",
        "Raw `todo_write`\r\n`ToolResult.metadata`",
        "Raw `todo_write`\n`ToolResult.metadata`",
        "provider-visible output text for `todowrite` was still only `Plan updated`",
        "provider-visible output text for `read` was the raw file content",
        "open requested-work targets remained `README.md`, `space_invader.py`, and",
        "Extend preflight so `read` / `supporting_context` and `todowrite` / `progress_projection` outputs",
        "Required Vision Route B case2a created `README.md`",
        "repeated\r\n`read(space_invader.py)` outputs",
        "repeated\n`read(space_invader.py)` outputs",
        "the block did not name `test_space_invader.py`",
        "active_targets: README.md, space_invader.py, test_space_invader.py",
        "repeated identical `todowrite` progress-projection outputs",
        "satisfying_progress_tools = write/apply_patch/equivalent file-change evidence",
        "supporting_projection_tools = read/list/search/todowrite",
        "says `todowrite` remains a valid tool output",
        "provider request still forced `tool_choice=required`",
        "tool_choice=auto unless an explicit policy requests required",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_closeout_vision_hook_exact_surface_domain = [
        "Codex standard Responses requests set `tool_choice` to `auto`",
        "Remove automatic `open_executable_work_requires_tool_call -> ToolChoice::Required`",
        "work uses `auto` sampling plus lifecycle / harness evidence",
        "Required Vision Route B case2a completed the runtime turn",
        "missing `README.md` and `test_space_invader.py`, failed `python -m unittest`",
        "This allows an early `python -m unittest` failure",
        "With provider\r\n`tool_choice=auto`",
        "With provider\n`tool_choice=auto`",
        "The last provider request had seven image attachments",
        "case image attachments when required",
        "Required Vision Route B no longer failed on image rediscovery",
        "failed `python -m unittest` evidence state",
        "repair `space_invader.py`",
        "use write/apply_patch to create or update missing artifacts",
        "`docling_convert(path=\"js-space_invaders01.jpg\")` failed",
        "repeated `glob(\"*.jpg\")` returned no matches",
        "Change manual ST case2 visible request wording from filename authority",
        "the next response must use `write` / `apply_patch` or equivalent file-changing evidence",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_provider_replay_progress_exact_surface_domain = [
        "verification_failed=[python -m unittest]",
        "repair_targets=[space_invader.py]",
        "tool_choice=none",
        "Tool schemas remain the stable interface and `tool_choice` remains `auto`",
        "Historical `write` calls for inactive targets",
        "current active requested-work targets are `test_space_invader.py`",
        "submitted a new `write` call with",
        "AssistantToolCalls(write, arguments={path",
        "historical `todowrite` tool-call payload",
        "active requested-work targets had rotated to `README.md`",
        "The model followed that stale plan and repeatedly submitted",
        "TodoWrite / progress projection item:",
        "provider-visible satisfying surface is write/apply_patch",
        "read/list/search remain supporting context",
        "Remove `todowrite` from the provider-visible effective tool set",
        "created `space_invader.py`, correctly rotated active requested-work targets",
        "submitted three more `write` calls",
        "WrongAuthoringTarget write call:",
        "broad write is saturated",
        "apply_patch remains available for remaining deliverables",
        "including `tool_choice=auto` / broad-surface contexts",
        "operation_bound_effective_tool_names` still removed `todowrite`",
        "runtime can dispatch todowrite",
        "first productive action is write/apply_patch",
        "The post-FR10-023 route created `space_invader.py`",
        "write(space_invader.py)",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_repair_label_target_exact_surface_domain = [
        "The failing `case2c` verification output",
        "python -m unittest\r\n  -> public_state_assertion_mismatch",
        "python -m unittest\n  -> public_state_assertion_mismatch",
        "active_targets include BEH prose and test_space_invader.py",
        "source-owned operation exact target falls back to test_space_invader.py",
        "generated-test refs can identify the failing test context",
        "case5 docs-only task",
        "README.md/basic_design.md/detail_design.md",
        "deliverable coverage = README/basic/detail",
        "FR10-028's root fix successfully moved `case5`",
        "active target = README.md",
        "read/list/inspect fills an unsatisfied slot",
        "write/apply_patch to active deliverable",
        "DocsRepair(active_targets = README.md/basic_design.md/detail_design.md)",
        "tool surface / tool_choice",
        "avoids write/apply_patch",
        "the next productive lifecycle item is `write` or\r\n  `apply_patch`",
        "the next productive lifecycle item is `write` or\n  `apply_patch`",
        "broad read/list/search is\r\n  saturated",
        "broad read/list/search is\n  saturated",
        "do not execute more broad read/list/search",
        "output includes active docs targets and write/apply_patch recovery",
        "exposed `read`,\r\n`list`, `grep`, `glob`, `inspect_directory`, `skill`, `docling_convert`, and `mcp_call`",
        "exposed `read`,\n`list`, `grep`, `glob`, `inspect_directory`, `skill`, `docling_convert`, and `mcp_call`",
        "`write` / `apply_patch` remain available",
        "Writing `README.md` or `basic_design.md`",
        "while `detail_design.md` remains open",
        "broad read/list/search after each partial write",
        "recovery surface remains write/apply_patch for remaining docs targets",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_recovery_repair_surface_exact_domain = [
        "the next provider-visible surface is still `write` / `apply_patch`",
        "`todowrite` remains visible as a non-satisfying progress",
        "`write` / `apply_patch` remain the only satisfying file-change progress",
        "Once `README.md` satisfied its required topics",
        "`basic_design.md` as the active docs focus",
        "A later `write(README.md)` was still accepted",
        "where a completed `README.md` is not allowed",
        "single known missing deliverable, `docs/calculator-design.md`",
        "repeated `list` calls as `supporting_context` / `no_progress`",
        "read/list/grep/inspect -> supporting_context no_progress",
        "keep write/apply_patch as satisfying file-change tools",
        "keep todowrite as a bounded progress side-channel",
        "Keep `write`, `apply_patch`, and `todowrite` visible",
        "repair intent forbids stale shell",
        "provider-visible surface = target-grounding read + write/apply_patch + todowrite",
        "list/search/shell are not advertised for this turn",
        "provider-visible surface = write/apply_patch + todowrite until",
        "provider-visible surface narrows to shell for the recorded verification command",
        "stale `shell`",
        "A target-scoped `read` is not a satisfying repair operation",
        "keeps `read` for target grounding, excludes `shell`",
        "compiled a shell-only\r\n`python -m unittest` action authority",
        "compiled a shell-only\n`python -m unittest` action authority",
        "`calculator.py` / `test_calculator.py` state",
        "no `FileChange` existed after the latest user request for\r\n`calculator.py` / `test_calculator.py`",
        "no `FileChange` existed after the latest user request for\n`calculator.py` / `test_calculator.py`",
        "require write/apply_patch or equivalent file-changing tool before final answer",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_runtime_patch_verification_reference_exact_domain = [
        "Case1 generated\r\n`calculator.py` and `test_calculator.py`, and `python -m unittest` passed",
        "Case1 generated\n`calculator.py` and `test_calculator.py`, and `python -m unittest` passed",
        "active target had drifted to `loader.py`",
        "`C:\\Python313\\Lib\\unittest\\loader.py`",
        "the basename `loader.py` as a source ref",
        "module `calculator.py`, and no `loader.py` existed",
        "repair lane may require write/apply_patch for module.py",
        "must filter Python runtime frames",
        "`calculator.py` in this\r\n  failure class",
        "`calculator.py` in this\n  failure class",
        "`unittest.loader.py` never becomes an open repair target while `calculator.py` remains",
        "requested implementation state: `calculator.py` and `test_calculator.py`",
        "external `python -m unittest` passed",
        "submitted an\r\n`apply_patch` for `docs/calculator-design.md`",
        "submitted an\n`apply_patch` for `docs/calculator-design.md`",
        "`--- Content is already up to date ---`",
        "apply_patch/update is an implicit full rewrite",
        "specific phrase `Content is already up to date`",
        "provider transport decode error",
        "`*** Update File: docs/calculator-design.md` sections",
        "External `python -m unittest` still failed",
        "apply_patch parser:",
        "apply_patch application:",
        "GUI manual ST\r\ncase3 stage3 reached `session_completed`, but external `python -m unittest`",
        "GUI manual ST\ncase3 stage3 reached `session_completed`, but external `python -m unittest`",
        "no post-change `shell` verification event",
        "`required_commands=[\"python -m unittest\"]`",
        "ProcessPhase::Verify with exact shell command authority",
        "GUI manual ST\r\ncase3 stage3 completed, but external `python -m unittest`",
        "GUI manual ST\ncase3 stage3 completed, but external `python -m unittest`",
        "The\r\nartifact stream showed `apply_patch` payload:",
        "The\nartifact stream showed `apply_patch` payload:",
        "Tool lifecycle recorded a destructive\r\n`FileChange` for `docs/calculator-design.md`",
        "Tool lifecycle recorded a destructive\n`FileChange` for `docs/calculator-design.md`",
        "until a fresh passed shell item appears",
        "updated\r\n`calculator.py`, `docs/calculator-design.md`, and `test_calculator.py`",
        "updated\n`calculator.py`, `docs/calculator-design.md`, and `test_calculator.py`",
        "post-write `python -m unittest` item existed",
        "ProcessPhase::Verify and ActionAuthority shell:<exact command>",
        "post-failure writes must",
        "according to\r\n`docs/calculator-design.md`",
        "according to\n`docs/calculator-design.md`",
        "`docs/calculator-design.md` as the only\r\nremaining authoring target",
        "`docs/calculator-design.md` as the only\nremaining authoring target",
        "A later `test_calculator.py` edit was rejected",
        "reference_inputs = [docs/calculator-design.md]",
        "deliverable_targets = [calculator.py, test_calculator.py]",
        "前回作成した docs/calculator-design.md をもとに、設計書だけを更新",
        "projected verification with `shell` as the only allowed tool",
        "previous docs/calculator-design.md をもとに design document only update",
        "deliverable_targets = [docs/calculator-design.md]",
        "structured documents were converted through `docling_convert`",
        "`docs.md`",
        "command = python -m unittest",
        "Python text I/O is UTF-8",
        "emits CLI-visible Japanese text",
        "tests capture subprocess output with encoding=\"utf-8\"",
        "The fix must preserve FR10-2026-05-18-022 shell display behavior: moyAI may normalize its shell",
        "changed only `docs/calculator-design.md`",
        "unknown two-token CLI input",
        "literal calculator command or `log 10`",
        "checks candidate Markdown from `write` / `apply_patch`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_public_command_delta_verification_exact_surface_domain = [
        "generated unit tests can pass while the public command surface",
        "stage3 runtime completed and `python -X utf8 -m unittest`",
        "required unittest or equivalent internal test result",
        "case names, calculator commands, and\r\n  Python-specific strings",
        "case names, calculator commands, and\n  Python-specific strings",
        "CLI-first rerun generated the required artifacts",
        "public API\r\nsymbols such as `sys.argv`",
        "public API\nsymbols such as `sys.argv`",
        "wrong item surface for `apply_patch`",
        "write tool call:\r\n  candidate artifact",
        "write tool call:\n  candidate artifact",
        "apply_patch tool call:\r\n  candidate artifact",
        "apply_patch tool call:\n  candidate artifact",
        "not calculator, case3, or one exact command string",
        "case1 before GUI",
        "`test_calculator.py`. The model edited `calculator.py`",
        "workspace then passed\r\n`python -X utf8 -m unittest`",
        "workspace then passed\n`python -X utf8 -m unittest`",
        "pending shell tool for\r\n`python -X utf8 -m unittest`",
        "pending shell tool for\n`python -X utf8 -m unittest`",
        "provider-visible surface = shell",
        "ActiveWorkContract::Verification(commands=[\"python -m unittest\"])",
        "`widget.py --probe`",
        "producer was\r\n`write`, `apply_patch`, shell mutation detection",
        "producer was\n`write`, `apply_patch`, shell mutation detection",
        "A `write` to the generated\r\ntest target",
        "A `write` to the generated\ntest target",
        "unittest-shaped content",
        "repeated `apply_patch` context mismatch",
        "correct `write` /\r\n  `apply_patch`",
        "correct `write` /\n  `apply_patch`",
        "case3/calculator-specific route branch",
        "final workspace passed unittest",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_repair_target_transcript_byproduct_exact_surface_domain = [
        "keeps the shell command obligation",
        "rather than case3,\r\n  calculator, or a particular Markdown filename",
        "rather than case3,\n  calculator, or a particular Markdown filename",
        "`FR10-2026-05-21-017` appeared during the CLI-first rerun",
        "exact verification shell\r\nreruns stayed disallowed",
        "exact verification shell\nreruns stayed disallowed",
        "not case1, calculator, `Popen`, or a raw unittest string",
        "invalid apply_patch arguments require recovery before terminal guard",
        "Case1 passed, which\r\nconfirms",
        "Case1 passed, which\nconfirms",
        "Case3 stage3\r\nthen failed",
        "Case3 stage3\nthen failed",
        "workspace already contained `calculator.py`, `docs/calculator-design.md`, and\r\n`test_calculator.py`",
        "workspace already contained `calculator.py`, `docs/calculator-design.md`, and\n`test_calculator.py`",
        "route-owned `python -m unittest` command passed",
        "`apply_patch` rejection / runtime failure",
        "invalid apply_patch/write call",
        "`write` and `apply_patch` invalid arguments",
        "GUI case1 stayed in verification after `python -X utf8 -m unittest` passed",
        "[\"python -m unittest\"]",
        "FileChange(calculator.py/test_calculator.py)",
        "FileChange(__pycache__/compiled cache) generated by verification",
        "ToolOutput(shell pass, changed_files=[change ids])",
        "`project_sandbox/codex_python_calc.md`",
        "CLI Required Core Route A\r\nwas green",
        "CLI Required Core Route A\nwas green",
        "`shell` remained disallowed under an edit-only surface",
        "`calculator.py` and `test_repair_allowed=false`",
        "not case3, calculator, or `python -m unittest`",
        "In case1, the failed verification\r\ncluster",
        "In case1, the failed verification\ncluster",
        "`test_calculator.py` syntax fix",
        "`calculator.py`, rejected\r\nthe generated-test patch",
        "`calculator.py`, rejected\nthe generated-test patch",
        "compiled shell-only verification",
        "generated-test write/apply_patch is current repair progress",
        "case1/calculator/Python syntax text",
        "generated-test `write` calls",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_source_generated_test_active_target_exact_surface_domain = [
        "generic `tool.py` public command evidence",
        "calculator, case3, Japanese output text, or `python -m unittest`",
        "Required Core Route A case3 stage3",
        "public CLI usage-error checks expected exit code 1",
        "concrete repair targets",
        "not by calculator, the public CLI strings, or unittest",
        "apply_patch(generated test)",
        "not the calculator task, the `os` symbol, or a specific manual ST\r\ncase",
        "not the calculator task, the `os` symbol, or a specific manual ST\ncase",
        "use case1, calculator, `test_divide_by_zero`, or one raw\r\n`assertLogs` line",
        "use case1, calculator, `test_divide_by_zero`, or one raw\n`assertLogs` line",
        "uses calculator filenames, a specific missing symbol, or a manual ST\r\ncase id",
        "uses calculator filenames, a specific missing symbol, or a manual ST\ncase id",
        "successful apply_patch(test artifact)",
        "write / apply_patch / todowrite",
        "`sqrt(-1)`",
        "target = test_calculator.py",
        "required verification = python -m unittest",
        "write(test_*.py)",
        "subprocess.run(input=...)",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_verification_continuation_closeout_exact_domain = [
        "post-fix CLI rerun passed case1 and reached case3 stage3",
        "python -m unittest` and `python -X utf8 -m unittest",
        "calculator, `pow`, `8 +`, or a case-specific oracle",
        "stopped in Required Core Route A case1",
        "by calculator, `parse_expression`, `operands`, or a unittest label",
        "proved case1 no longer recurred, then stopped in Required Core Route\r\nA case3",
        "proved case1 no longer recurred, then stopped in Required Core Route\nA case3",
        "source-owned exact target authority for `calculator.py`",
        "not calculator,\r\n  `test_wrong_arg_count`, localized usage wording",
        "not calculator,\n  `test_wrong_arg_count`, localized usage wording",
        "passed case1 and then exposed a separate case3 stage2 failure",
        "`python tool.py <mode> <input>`",
        "run_cli(\"status\", \"file.txt\")",
        "special allowance for calculator, `pow`, function names, or stage2 wording",
        "generic `tool.py` command-family templates",
        "helper-composed subprocess calls",
        "post-fix CLI rerun passed case1 and then exposed a separate case3 failure",
        "deliverables = widget.py + docs/widget-design.md + test_widget.py",
        "docs/test can be reopened even though they are evidence inventory",
        "post-fix CLI rerun passed case1 and then exposed a route-closeout failure in case3",
        "`verification_command_log.json` entry",
        "python -m unittest passed at t=20",
        "command=python -X utf8 -m unittest",
        "satisfies_command_identities=[python -m unittest]",
        "not keyed by\r\n  calculator, stage2, or one UTF-8 wrapper",
        "not keyed by\n  calculator, stage2, or one UTF-8 wrapper",
        "public CLI stdout/stderr assertion evidence",
        "unittest failures named `result.stdout` / `result.stderr`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_output_path_stream_read_stop_hook_exact_domain = [
        "self.assertIn(\"error\", result.stderr)",
        "source_refs = [\"error\"]",
        "test_refs = [\"test_tool.py\"]",
        "generic `tool.py` / `test_tool.py` evidence",
        "never keys the gate by\r\n  calculator, localized strings, or one manual ST case",
        "never keys the gate by\n  calculator, localized strings, or one manual ST case",
        "post-fix CLI rerun passed case1 and then failed in case3 stage1",
        "..\\project_sandbox\\...\\case3\\workspace",
        "C:\\Users\\...\\project_sandbox\\...\\case3\\workspace\\docs\\calculator-design.md",
        "not keyed by calculator, case3, or `docs/calculator-design.md`",
        "failed earlier in\r\ncase1 after the repair item",
        "failed earlier in\ncase1 after the repair item",
        "active target = widget.py",
        "provider-visible tools = apply_patch / todowrite / write",
        "manual ST\r\n  case id, calculator filenames",
        "manual ST\n  case id, calculator filenames",
        "before a content-changing `write` or `apply_patch`",
        "narrows to write/apply_patch/todowrite-only",
        "failed in case3 stage3 after a\r\nverification failure",
        "failed in case3 stage3 after a\nverification failure",
        "test_refs = [test file]",
        "localized stdout examples",
        "active target = source file",
        "active_targets = [generated test file, source file]",
        "not manual ST case3, Python unittest, calculator files, or one\r\n  localized output string",
        "not manual ST case3, Python unittest, calculator files, or one\n  localized output string",
        "post-fix CLI rerun passed case1 and then failed in case3 stage3 before GUI",
        "manual ST harness had already created the correct text-only verification-repair\r\ncontinuation",
        "manual ST harness had already created the correct text-only verification-repair\ncontinuation",
        "Manual ST verification-repair continuation",
        "Repair targets: calculator.py",
        "Requested deliverables still require authoring in the workspace: calculator.py",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_subprocess_command_repair_exact_domain = [
        "The GUI-generated `test_calculator.py` executed child CLI commands with",
        "Any generated Python test target (`test_<module>.py` or `<module>_test.py`)",
        "This contract is enforced before `write` / `apply_patch` applies filesystem side effects",
        "Keep the fixture generic (`tool.py` / `test_tool.py`)",
        "`python -X utf8 calculator.py 8 +` could be rendered as required",
        "`typed required action projection=shell:python -m unittest`",
        "submitted a diagnostic `Get-ChildItem` command",
        "not by case1 or\r\n  `python -m unittest`",
        "not by case1 or\n  `python -m unittest`",
        "provider-corrupted payload with embedded write-argument authority",
        "content-only `write` payload",
        "case1, calculator, or one syntax-error string",
        "The provider submitted `shell` verification reruns",
        "required `write` / `apply_patch`\r\n  to the exact generated-test target",
        "required `write` / `apply_patch`\n  to the exact generated-test target",
        "A submitted `shell` command whose identity belongs to remaining verification",
        "not by calculator, vLLM-MLX, or one\r\n  shell command string",
        "not by calculator, vLLM-MLX, or one\n  shell command string",
        "The post-repair shell run failed",
        "post-repair generated-test public-output overreach",
        "`python -m unittest test_widget -v` extending `python -m unittest`",
        "cwd wrappers, `-X utf8`, module selectors",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_recovery_provider_finalmessage_path_exact_domain = [
        "used named `write` tool choice and excluded `shell`",
        "schema-outside `shell` reruns again",
        "`apply_patch` and `write` are visible, `read` / `todowrite` / `shell` are hidden",
        "only `apply_patch` and `write` were visible and `tool_choice=required`",
        "a rejected `shell`\r\n  verification rerun records",
        "a rejected `shell`\n  verification rerun records",
        "previous stale shell\r\nrejection narrowed repair dispatch",
        "previous stale shell\nrejection narrowed repair dispatch",
        "still narrowed tools to `apply_patch` / `write`",
        "OpenAI-compatible vLLM-MLX provider",
        "provider ignores named `write` and submits `shell`",
        "verify phase and named shell dispatch",
        "correction text names the\r\n  `shell` tool",
        "correction text names the\n  `shell` tool",
        "correction text continues to name `write` / `apply_patch`",
        "stale shell\r\nverification even though `shell` is forbidden",
        "stale shell\nverification even though `shell` is forbidden",
        "literal verification command text",
        "The runtime wrote\r\n`calculator.py`, then the model asked `glob`",
        "The runtime wrote\n`calculator.py`, then the model asked `glob`",
        "`glob` output should prefer workspace-relative labels so later `read` / `write` targets",
        "named `write` while still\r\nshowing the broad stable tool schema",
        "named `write` while still\nshowing the broad stable tool schema",
        "First authoring dispatch remains Codex-style broad candidate surface with `tool_choice=auto`",
        "authoring tools: `write` and `apply_patch`",
        "recovery uses the same rule shape but narrows to `shell`",
        "strict\r\n`write` / `apply_patch`",
        "strict\n`write` / `apply_patch`",
        "rerunning\r\n`python -m unittest` after repair",
        "rerunning\n`python -m unittest` after repair",
        "`required_verification_commands_after_repair`",
        "point back to `write` / `apply_patch`",
        "executed `python -m unittest` through a direct PowerShell path",
        "Shift_JIS / CP932",
        "not by `case1`, calculator, or `python -m unittest`",
        "single missing artifact, `test_calculator.py`",
        "`typed required action projection` remained absent",
        "must be `write:<target>`",
        "must not use case1, calculator, or a fixed test file",
        "`typed required action projection=write:test_calculator.py`",
        "rejected `inspect_directory` ToolResult",
        "must not branch on `inspect_directory`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_artifact_shape_language_specific_authority_drift = [
        "The Python test content-shape contract includes subprocess output capture authority",
        "`CompletedProcess.stdout` / `.stderr`",
        "`apply_patch` admission for `test_*.py` / `*_test.py`",
        "python_test_module_content_shape",
        "For test targets inferred from `test_*.py` / `*_test.py`",
        "valid Python syntax, but it is not an executable test module",
        "executable Python-test module shape classifier",
        "For production Python source targets",
        "generic Python source executable-content classifier",
        "The provider wrote `docs/calculator-design.md`",
        "Python source, generated tests, and text artifacts",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_source_test_grounding_recovery_language_surface_drift = [
        "root-level `test_*.py`, `*_test.py`, `.test.*`, or `.spec.*` files",
        "Japanese `テスト` and `unittest` prose",
        "JSON/Python-escaped serialized Markdown/text",
        "Python source content-shape validation is also repair contract authority",
        "`python_source_executable_content_shape` is both rejection metadata",
        "required Python source\r\n  shape for the active target",
        "required Python source\n  shape for the active target",
        "effective Python\r\n  module text",
        "effective Python\n  module text",
        "JSON/Python-escaped serialized source",
        "same test-module, text-artifact, and Python-source content-shape renderers",
        "A `read`, `list`, `grep`, `glob`, or `inspect_directory` proposal under exact edit repair",
        "`typed required action projection=write:calculator.py`",
        "`apply_patch` / `read` / `write`",
        "`component.py` / `calculator.py` was already read",
        "satisfies `python_source_executable_content_shape`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_multitarget_docs_recovery_exact_surface_drift = [
        "read(active target) -> supporting_context evidence",
        "keep write/apply_patch as satisfying edit proposal tools",
        "not invent a `write:<target>` string",
        "Broad read/list/\r\ngrep/shell/todowrite remain forbidden",
        "Broad read/list/\ngrep/shell/todowrite remain forbidden",
        "expose read/write/apply_patch plus list",
        "typed required action projection=write:<target>",
        "provider-visible surface: read/write/apply_patch/(todowrite when available)",
        "satisfying progress: write/apply_patch only",
        "bounded grounding progress: read(exact target)",
        "stale read/search/shell proposals",
        "`list`, `grep`, `glob`, `shell`, and non-target reads remain out of the surface",
        "`satisfying_tools`: `write` / `apply_patch`",
        "supporting `read` / `inspect_directory` / `grep` / `list` history",
        "Use `tool_choice=required` for this narrowed docs/text recovery",
        "Preserve stable authoring tools and `tool_choice=auto`",
        "case1, calculator, vLLM-MLX",
        "Source-only grep/read evidence",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_stable_surface_plan_sidechannel_exact_surface_drift = [
        "`Scenario contract authority` / `contract references` sections remain",
        "Same-document update aliases such as `いま作成した設計書を更新`",
        "`scenario_contract.md/json` solely because those",
        "The satisfying progress surface is still `write` / `apply_patch`",
        "`todowrite` remains provider-visible as a non-satisfying progress side-channel",
        "Required action projection may still say `write:<target>`",
        "The next provider request uses `tool_choice=auto`",
        "`shell`, `read`, `list`, and other support tools are not removed",
        "Narrowed `tool_choice=required` surfaces remain reserved",
        "provider-specific vLLM-MLX handling, case1/calculator branches",
        "same stable surface and `tool_choice=auto`",
        "Do not add case1, calculator,\r\n  vLLM-MLX",
        "Do not add case1, calculator,\n  vLLM-MLX",
        "`bounded_code_authoring_recovery_tool_choice()`",
        "assert `auto`",
        "provider-specific prompt wording as the primary fix",
        "`write(path, content)` JSON function call",
        "provider-visible tool schemas omit `write`",
        "keep `apply_patch` as the edit surface",
        "stable registry re-expansion cannot reintroduce whole-file `write`",
        "normal repair dispatch also\r\n  omits whole-file `write`",
        "normal repair dispatch also\n  omits whole-file `write`",
        "`write` may remain readable as historical protocol evidence",
        "malformed `write` JSON",
        "`codex_style_code_authoring_omits_whole_file_write_fixture_passes()`",
        "Codex does not expose `list(include_hidden: bool)`",
        "`apply_patch`, `shell`, and `todowrite` only",
        "`list`, `read`, `grep`, `glob`, `inspect_directory`",
        "guidance says `apply_patch`, not\r\n  `write/apply_patch`",
        "guidance says `apply_patch`, not\n  `write/apply_patch`",
        "retain only `apply_patch`, `shell`, and\r\n  `todowrite`",
        "retain only `apply_patch`, `shell`, and\n  `todowrite`",
        "hard-coded `write/apply_patch`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_repair_activework_inactive_snapshot_exact_surface_drift = [
        "not calculator, case1, or\r\n  provider-specific text",
        "not calculator, case1, or\n  provider-specific text",
        "Shell remains part of the Codex core Code surface",
        "If the shell command is a bounded file inspection",
        "If the shell command inspects or probes a different target",
        "ToolResult feedback must not mention omitted provider tools such as whole-file `write`",
        "project either `apply_patch` with exact target/context guidance or a\r\n  bounded shell inspection",
        "project either `apply_patch` with exact target/context guidance or a\n  bounded shell inspection",
        "not `run unittest now`",
        "after the Code surface moved away from whole-file `write` and toward Codex-style\r\n`apply_patch`",
        "after the Code surface moved away from whole-file `write` and toward Codex-style\n`apply_patch`",
        "A successful `apply_patch` / FileChange for an inactive target",
        "not from legacy `write(path, content)`",
        "must not reintroduce whole-file `write`",
        "successful inactive `apply_patch` pair",
        "`calculator.py` was accepted through a `shell` command",
        "active target rotated to `test_calculator.py`",
        "not a case or\r\n  calculator-specific route condition",
        "not a case or\n  calculator-specific route condition",
        "A later `NO TESTS RAN` cluster",
        "malformed `apply_patch` no-progress items",
        "Codex keeps normal implementation turns on `tool_choice=auto`",
        "contains `apply_patch` but not whole-file `write`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_authoring_finalmessage_exact_surface_drift = [
        "`DocsRepair` with active target `docs/calculator-design.md`",
        "keeps `apply_patch` as the satisfying edit primitive",
        "keeps `shell` / `todowrite` plus scoped grounding tools available",
        "non-Codex JSON discovery tools such as `list`, `glob`, and `inspect_directory`",
        "`apply_patch:docs/calculator-design.md`",
        "`apply_patch`, `shell`, `todowrite`, and scoped grounding tools `read`, `grep`, `docling_convert`, `mcp_call`",
        "Dispatch remains `tool_choice=auto`; exact `apply_patch:<docs-target>`",
        "No empty-tool-name special retry",
        "allowed_tools=[apply_patch]` and `tool_choice=required`",
        "preserves the docs stable surface or the exact target-grounding/edit surface",
        "retained scoped grounding tools",
        "For `calculator.py, test_calculator.py` with `apply_patch` visible and `write` omitted",
        "one `patch_text` may contain multiple file sections",
        "must not force provider `tool_choice=required`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_recovery_projection_exact_surface_drift = [
        "The correction retains active targets, required edit shape, stable surface, and normal `tool_choice=auto`",
        "multi_file_apply_patch_shape",
        "`tool patch error` from `apply_patch`",
        "allowed surface, `tool_choice`, and side-effect-free no-progress metadata",
        "preserves normal `tool_choice=auto` unless TurnControlEnvelope",
        "strict `apply_patch` grammar",
        "strict_apply_patch_grammar",
        "add_file_line_prefix_rule",
        "malformed `apply_patch` failures",
        "malformed `todowrite` arguments while `calculator.py` and `test_calculator.py` remained open",
        "`todowrite` is a non-satisfying progress side-channel",
        "Malformed `todowrite` / schema-invalid model actions",
        "same malformed `apply_patch` family",
        "`candidate_target_from_arguments=calculator.py`",
        "`Open targets: calculator.py, test_calculator.py`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_hard_recovery_system_authority_exact_surface_drift = [
        "`New-Item`, `test_calculator.py`, and `NO TESTS RAN` are evidence",
        "OpenAI-compatible-only payloads split provider language/final-answer policy",
        "Stable tool surface and `tool_choice=auto` remain the normal Codex-style behavior",
        "not by case1, calculator filenames, vLLM-MLX wording",
        "after malformed `apply_patch` recovery was added",
        "Required Core case1 terminalized",
        "`open_obligation_final_message_recovery` markers and stable Code tools",
        "kept `tool_choice=auto` even after repeated recovery-visible final text",
        "first Code Author final-message recovery keeps Codex-style stable surface and `tool_choice=auto`",
        "narrowed to exactly `apply_patch` and `write`",
        "`tool_choice=required` decision",
        "Re-augment `apply_patch` / `write`",
        "exposed `apply_patch` / `write` with `tool_choice=required`",
        "normal Code authoring expose broad `write`",
        "Docs malformed apply_patch uses the same bounded OpenAI-compatible fallback",
        "terminalized on side-effect-free malformed `apply_patch` because the OpenAI-compatible bounded write fallback was scoped to Code only",
        "`apply_patch` is a freeform custom tool in Codex",
        "does not encode a document-sized patch inside a JSON `patch_text` field",
        "case3 correctly started as Docs Author",
        "effective surface became `apply_patch` required",
        "generated a Markdown document in `patch_text`",
        "produced `apply_patch_malformed_patch` three times",
        "`malformed_apply_patch_write_recovery_surface` predicate required `TaskRoute::Code`",
        "retained only `apply_patch`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_grounding_content_budget_exact_surface_drift = [
        "provider-visible surface dropped exact-target grounding",
        "stale shell/read style action for `calculator.py`",
        "`read` / `write` / `apply_patch` / `todowrite` from the stable tool registry",
        "keeps bounded exact-target `read` alongside `write` / `apply_patch` / `todowrite`",
        "`shell`, broad discovery tools, and case/provider-specific retry paths remain excluded",
        "`python tool.py log 10`",
        "`未知の 2 トークン CLI`",
        "not case3, calculator filenames, or one provider output string",
        "`SyntaxError` / parse defect",
        "already named `calculator.py`",
        "kept only `apply_patch` visible",
        "rejected repeated provider `shell` orientation",
        "`active_targets=[test_calculator.py]` and `typed required action projection=apply_patch:test_calculator.py`, but `allowed_tools=[apply_patch]`",
        "`tool_choice` remains owned by `TurnLifecycleKernel`",
        "Required Core case1 accepted a Markdown/spec body as `calculator.py`",
        "`calculator.py` was therefore",
        "`invalid_edit_arguments` recovery",
        "Malformed `write` / `apply_patch` arguments",
        "vLLM-MLX configured at 131072",
        "case1, calculator, `NO TESTS RAN`, vLLM-MLX",
        "Required Core case1 accepted a valid `calculator.py`",
        "malformed `write(test_calculator.py)`",
        "dispatch envelope stayed on generated-test grounding surface with `tool_choice=auto`",
        "`malformed_write_patch_capable_recovery_surface`",
        "re-expanding `read` / `shell` / `todowrite` and leaving `tool_choice=auto`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_wrongtarget_closeout_legacywrite_exact_surface_drift = [
        "Required Core case1 reached malformed edit recovery for open generated-test target",
        "`test_calculator.py`, then received side-effect-free `wrong_authoring_target` outputs",
        "`write(calculator.py)` calls",
        "`apply_patch` / `write`, `tool_choice=required`, active",
        "current `write:test_calculator.py`",
        "failed inactive `write` / `apply_patch` ToolCalls",
        "Required Core case1 externally ran `python -m unittest`",
        "`test_calculator.py` was still missing",
        "executed\r\n  `python -m unittest`, producing `NO TESTS RAN`",
        "executed\n  `python -m unittest`, producing `NO TESTS RAN`",
        "reported `calculator.py` missing",
        "not by case1,\r\n  calculator filenames, `NO TESTS RAN`, or vLLM-MLX behavior",
        "not by case1,\n  calculator filenames, `NO TESTS RAN`, or vLLM-MLX behavior",
        "Required Core case1 rotated active requested-work to `test_calculator.py`",
        "`write(calculator.py)` calls while `test_calculator.py` remained missing",
        "whole-file JSON `write` primitive",
        "`typed required action projection=apply_patch:test_calculator.py`",
        "reintroduced `write` from stable schemas",
        "production-source `write(calculator.py)`",
        "must not expose whole-file `write(path, content)`",
        "`apply_patch` is the satisfying edit primitive for singleton missing generated-test targets",
        "`read` may be restored only for bounded target grounding. `shell` and `todowrite` remain",
        "provider-portable `write` recovery only",
        "not by case1,\r\n  calculator filenames, vLLM-MLX, or a wrong-target string",
        "not by case1,\n  calculator filenames, vLLM-MLX, or a wrong-target string",
        "Remove `write` from generated-test source-reference grounding",
        "Required Core case3 stage1 created the docs deliverable",
        "case-level code/test artifacts",
        "rejected docs patches as wrong target",
        "`calculator.py` and `test_calculator.py`",
        "became new requested deliverables",
        "not by case3, calculator filenames, docs route wording, or provider behavior",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_progress_public_docs_provider_exact_surface_drift = [
        "narrows provider-visible tools to `apply_patch` as the satisfying edit surface",
        "provider-portable `tool_choice=required`",
        "`todowrite` and `shell` are removed only for this typed recovery dispatch",
        "not by case3, calculator filenames",
        "removed the bounded active-target `read`",
        "next request exposed only `apply_patch`",
        "proposed `shell Get-Content calculator.py`",
        "exact target `read` as grounding evidence",
        "ungrounded active targets expose `read` plus `apply_patch`",
        "`todowrite` and `shell` remain unavailable",
        "`tool_choice=required` and `plan_reason=progress_projection_edit_recovery`",
        "not case3, calculator, or the\r\n  provider's `Get-Content` spelling",
        "not case3, calculator, or the\n  provider's `Get-Content` spelling",
        "unsupported public input should raise `ValueError`",
        "source patch to\r\n`calculator.py`",
        "source patch to\n`calculator.py`",
        "repair target `test_calculator.py`",
        "`test_<module>.py` lost strict contract authority",
        "`assertRaises` / \"expected exception not raised\"",
        "for `docs/calculator-design.md`",
        "exposed named `write` for the same",
        "one claim id, or a provider-specific final-message behavior",
        "entered source-owned verification repair for\r\n`calculator.py`",
        "entered source-owned verification repair for\n`calculator.py`",
        "proposed `shell`",
        "under `apply_patch/write` authority",
        "re-expanded to `apply_patch` / `read` / `todowrite` / `write`",
        "The effective surface is `apply_patch` / `write`, not `read` / `todowrite` / `shell`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_wrongtarget_publiccommand_invalidedit_exact_drift = [
        "route-owned `python -m unittest` green",
        "failed in case1 after creating `calculator.py`",
        "singleton `test_calculator.py`",
        "submitted `apply_patch` for inactive `calculator.py` three times",
        "no verification ran and `test_calculator.py`",
        "broad Code Author surface with `tool_choice=auto`",
        "failed inactive `apply_patch` class",
        "narrows\r\n  to `apply_patch` / `write`",
        "narrows\n  to `apply_patch` / `write`",
        "provider-portable required tool choice",
        "stale inactive `write` and\r\n  `apply_patch` payloads",
        "stale inactive `write` and\n  `apply_patch` payloads",
        "Required Core rerun-018 passed case1 and reached case3 stage3",
        "`python -m unittest` passed",
        "allowed tool surface were correct (`apply_patch:calculator.py`)",
        "raw EOFError tracebacks/full Windows paths",
        "`RepairOperationTemplate.required_evidence` authority",
        "source ownership, exact target, and a\r\n  direct argv-mode repair intent",
        "source ownership, exact target, and a\n  direct argv-mode repair intent",
        "raw traceback frames, full Windows paths",
        "Required Core rerun-019 passed case1 and reached case3 stage3",
        "updated `calculator.py` for pow / unary / argv behavior",
        "previous `python -m unittest`",
        "singleton\r\n`test_calculator.py` with `apply_patch` / `write`",
        "singleton\n`test_calculator.py` with `apply_patch` / `write`",
        "out-of-surface shell/list proposals",
        "invalid `apply_patch` payload started with an inactive `calculator.py`",
        "malformed `test_calculator.py` section",
        "candidate target\r\n  `calculator.py` and active target `test_calculator.py`",
        "candidate target\n  `calculator.py` and active target `test_calculator.py`",
        "active test edit",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_provider_generatedtest_docs_grounding_exact_drift = [
        "Required Core rerun-020 passed case1 and entered Docs authoring",
        "malformed `apply_patch` payloads twice",
        "`ToolChoice::Named(Write)`",
        "named write envelope",
        "Docs exact-write recovery case",
        "`apply_patch:test_calculator.py`",
        "provider-visible `read`\r\n  lane existed for the updated `calculator.py`",
        "provider-visible `read`\n  lane existed for the updated `calculator.py`",
        "strict `apply_patch` / `write` with required tool\r\n  choice",
        "strict `apply_patch` / `write` with required tool\n  choice",
        "`shell` and broad orientation tools remain unavailable",
        "Required Core rerun-022 failed in case1 after `test_calculator.py`",
        "`class TestAdd(FILE_ID, API_ID, BEH_ID):`",
        "Route-owned `python -m unittest`",
        "Test-target `write` / projected `apply_patch`",
        "entered `DocsRepair` for\r\n`docs/calculator-design.md`",
        "entered `DocsRepair` for\n`docs/calculator-design.md`",
        "surface with `apply_patch`, repository inspection tools, and `todowrite`",
        "strict `apply_patch` / `write`\r\nunder `progress_projection_edit_recovery`",
        "strict `apply_patch` / `write`\nunder `progress_projection_edit_recovery`",
        "keeps `apply_patch`, `read`, `grep`, `shell`, `docling_convert`, and `mcp_call`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_closeout_command_semantic_transport_exact_drift = [
        "Required Core rerun-024 passed case1 and entered case3 docs authoring",
        "`TaskRoute::Docs`, `DocsRepair`, and bounded grounding tools",
        "`docs/calculator-design.md` was still missing",
        "`TaskRoute::Code` / `RequestedWorkAuthoring`",
        "`apply_patch,todowrite`; source/test reads",
        "not by case3,\r\n  calculator filenames, or one provider read proposal",
        "not by case3,\n  calculator filenames, or one provider read proposal",
        "Required Core rerun-025 passed case1 and advanced case3",
        "`calculator.py` with `Get-Content calculator.py -Encoding UTF8`",
        "`python -m unittest` and other runtime/test\r\n  commands",
        "`python -m unittest` and other runtime/test\n  commands",
        "PowerShell's `UTF8` spelling",
        "path suffix token `py` from `calculator.py`",
        "`powershell_get_content_utf8_explicit`",
        "Runtime correctly rejected two `docs/calculator-design.md` patch drafts",
        "`python calculator.py 8 +` and",
        "`python calculator.py log 10`",
        "The executable surface may remain `apply_patch`",
        "not by case3, calculator\r\n  filenames, or a single hardcoded CLI example",
        "not by case3, calculator\n  filenames, or a single hardcoded CLI example",
        "creating source, test, and docs artifacts and after an initial `python -m unittest` pass",
        "authoring recovery for `calculator.py` and\r\n`test_calculator.py`",
        "authoring recovery for `calculator.py` and\n`test_calculator.py`",
        "`apply_patch/write` and `tool_choice=required`",
        "not by case3, stale\r\n  verification, calculator files, or one provider endpoint",
        "not by case3, stale\n  verification, calculator files, or one provider endpoint",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_docs_patch_budget_transport_semantic_exact_drift = [
        "`context mismatch`, `failed to find expected lines`",
        "`apply_patch_context_mismatch` before generic JSON `expected` parsing",
        "exposes `read`,\r\n  `apply_patch`, and bounded `write`",
        "exposes `read`,\n  `apply_patch`, and bounded `write`",
        "not by calculator docs wording",
        "including the\r\n  exact-target `write` lane",
        "including the\n  exact-target `write` lane",
        "Required Core rerun-029 passed case1 and then entered case3",
        "Case3 created source,\r\ntest, and docs artifacts",
        "Case3 created source,\ntest, and docs artifacts",
        "prompt-visible continuation budget says `n/6`",
        "no case3 route-owned verification command",
        "not by calculator artifacts, one failure\r\n  message, or local provider final-message behavior",
        "not by calculator artifacts, one failure\n  message, or local provider final-message behavior",
        "Required Core rerun-030 clean retry passed case1 and case3 stage1 verification",
        "case3 stage2 failed before completion",
        "`stream_max_retries=2`",
        "active docs artifact",
        "`unknown_two_token_cli_usage_error_exit_1` missing",
        "concept words such as unknown / two-token",
        "unknown two-token CLI usage-error claim",
        "incomplete_binary_cli_usage_error_exit_1",
        "exit-code header was localized",
        "localized table shape with an exit-code header",
        "localized Markdown table",
        "unknown_two_token_cli_usage_error_exit_1",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_generatedtest_invalidedit_samedoc_exact_drift = [
        "`test_calculator.py` asserted",
        "while `calculator.py` printed",
        "Python 3.13 inserted a caret line",
        "`public_output_stream_assertion_mismatch` inspected",
        "`assertIn` and `assertEqual` public-output assertion",
        "`inspect.getsource(<function>.__module__)`",
        "Required Core rerun-003 case1 created source/test/contract artifacts",
        "`python -X utf8 -m unittest`",
        "chose exact target `calculator.py`",
        "localized\r\n  message assertions unless the current contract names them",
        "localized\n  message assertions unless the current contract names them",
        "Invalid `apply_patch` feedback records all declared patch targets",
        "requires a target-only `apply_patch` or `write`",
        "invalid_edit_arguments:tool=apply_patch;submitted_targets=...",
        "alongside the exact `apply_patch:<active target>` required action",
        "Repeated stale `read` / `shell` proposals",
        "Required Core rerun-006 passed case1",
        "`docs/calculator-design.md` as active authoring work",
        "`DocsRouteState.pending_deliverables=[]`",
        "`route_contract_summary=\"docs route contract satisfied\"`",
        "mere presence\r\n  of `active_deliverable`",
        "mere presence\n  of `active_deliverable`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_closeout_content_shape_verdict_exact_drift = [
        "FR22 Required Core rerun-003 passed case1 and reached case3 stage3",
        "source-owned `calculator.py`",
        "`calculator.py` did not change after",
        "FR22 Required Core rerun-004 reached case1 source-owned repair for `calculator.py`",
        "`000021`, `000024`, and `000027`",
        "`000029_model_request_sent.json`",
        "`f69e1ba194f7b8767925219a140af939591692f45d6aababb36b29e91dfb08c7`",
        "malformed EOF write (`e430c384...`)",
        "exact `write:<target>` authority",
        "FR22 rerun-006 first runtime session",
        "not a `case1` or calculator\r\n  fixture key",
        "not a `case1` or calculator\n  fixture key",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_generated_subprocess_docsroute_source_vision_exact_drift = [
        "FR22 Required Core rerun-007 passed case1",
        "failed-edit repair tools (`apply_patch`, `write`,\r\n`todowrite`) without `shell` / `read`",
        "failed-edit repair tools (`apply_patch`, `write`,\n`todowrite`) without `shell` / `read`",
        "Required Core rerun-009 case1 created the expected source and generated test artifacts",
        "generated `test_calculator.py` used `sys.environ`",
        "provider repeatedly targeted inactive `calculator.py` while `test_calculator.py`",
        "write/apply_patch payload variations",
        "Required Core rerun-013 used FR22-012 post-fix gates",
        "malformed non-edit `read` invalid arguments",
        "Required Core rerun-014 passed case1 and case3 stage1",
        "pending deliverable `docs/calculator-design.md`",
        "`active_targets=[calculator.py]`",
        "repair `calculator.py` while active docs work still required",
        "`capture_output=True` is not sufficient by itself",
        "not to calculator\r\n  or BEH-5 specifically",
        "not to calculator\n  or BEH-5 specifically",
        "`self.assertEqual(result.returncode, 0)`",
        "`self.assertEqual(result.returncode, 0, f\"stdout={result.stdout!r} stderr={result.stderr!r}\")`",
        "Required Core rerun-016 case1 created `test_calculator.py`",
        "`NameError: name 'sys' is not defined`",
        "malformed `read`\r\ncalls while `test_calculator.py`",
        "malformed `read`\ncalls while `test_calculator.py`",
        "`sys`, `os`,\r\n  `subprocess`, and `unittest`",
        "`sys`, `os`,\n  `subprocess`, and `unittest`",
        "`sys.`, `os.`, `subprocess.`, and `unittest.` references",
        "Required Core rerun-017 は",
        "LM Studio Required Core rerun-020",
        "The final `calculator.py`",
        "`python_source_executable_content_shape` violations",
        "LM Studio Required Vision Route B full rerun case2a",
        "request diagnostics still showed the original image attached\r\n(`image_count=1`, `image_bytes=173444`)",
        "request diagnostics still showed the original image attached\n(`image_count=1`, `image_bytes=173444`)",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_later_vision_terminal_source_shape_exact_drift = [
        "Fresh Required Vision rerun follows.",
        "Required Vision Route B rerun-005",
        "`space_invader.py` edits while case2a authoring work remained open",
        "`Tool apply_patch returned no_progress output 3 time(s)`",
        "`tool_no_progress:<name>`",
        "generic `tool_no_progress:<tool>` parsing",
        "Required Vision Route B rerun-009",
        "`space_invader.py` の content-changing patch",
        "workspace fingerprint が変わったことを理由に",
        "`same verification failure evidence repeated` は `verification_non_convergence`",
        "Required Vision rerun-010",
        "`space_invader.py` 用の Python source",
        "candidate を `write` で送った",
        "`def rects_overlap` / `def _run_gui`",
        "`observed_forbidden_markers`",
        "`Required write content shape mismatch`",
        "test-target forbidden markers (`def <non-test>`, `def main`, invalid Test base など)",
        "`python_source_executable_shape_accepts_required_public_surface_fixture_passes`",
        "Required Vision rerun-011",
        "`space_invader.py` candidate の GUI helper",
        "`root.bind(...)` method-call lines",
        "Required Vision rerun-012",
        "`GameState.fire_player_bullet` 欠落と `rects_overlap` edge contact",
        "runtime は source-owned repair lane と `apply_patch:space_invader.py`",
        "`authoring_target_grounding_required` を3回検出",
        "Required Vision rerun-013",
        "malformed `apply_patch` を `invalid_edit_arguments`",
        "`space_invader.py` への `write` candidate",
        "`Tool write returned no_progress output 3 time(s) while content-changing authoring is required`",
        "`content_changing_authoring_no_progress:{tool}`",
        "Required Vision rerun-014",
        "`write` no-progress terminal後に route は `fail` / `route_terminalized`",
        "`raw prose line inside Python source` は `return not (`",
        "`source_boolean_comparison_continuation_allowed`",
        "Required Vision rerun-015",
        "`active_targets=[space_invader.py]`",
        "required action `apply_patch:space_invader.py`",
        "context-only `read(test_space_invader.py)` / `todo_write` / `read(space_invader.py)`",
        "artifact-first evidence では generated `test_space_invader.py`",
        "同じ test module を `python -m unittest test_space_invader -v`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_failed_inactive_wrongtarget_malformed_edit_exact_drift = [
        "failed inactive `write` / `apply_patch`",
        "`source.py` / `test_source.py` artifacts rather than manual ST case or calculator paths",
        "Required Core rerun-023 under LM Studio",
        "required action `apply_patch:<test target>`",
        "broad whole-file `write(path, content)`",
        "wrong-target recovery exposes `apply_patch`; generated-test source-reference recovery exposes",
        "Required Core rerun-024 passed case1",
        "`calculator.py` still used stdin prompting",
        "malformed `apply_patch` payloads",
        "`python -m unittest` still passed",
        "exposes `apply_patch` + `write`",
        "Required Core rerun-025",
        "`calculator.py` was authored, `test_calculator.py` remained missing",
        "model repeatedly submitted `apply_patch` targeting the",
        "failed inactive `write` evidence was",
        "malformed mixed-target `apply_patch` attempts",
        "malformed patch recovery was active from",
        "`active_generated_test_malformed_apply_patch_required_action_surface_alignment`",
        "Required Core rerun-035 used",
        "`write:test_calculator.py` action authority",
        "provider-visible `write.path` schema",
        "exact `write:<target>` action authority",
        "`exact_write_path_schema_projection`",
        "Required Core rerun-036 proved",
        "`write.path` enum [`test_calculator.py`]",
        "submitted `write(calculator.py)`",
        "`ArtifactContentShapeViolation`",
        "a representative rerun used FR22-015 post-fix gates",
        "exact `apply_patch:<target>` still relied",
        "`exact_apply_patch_wrong_path_content_shape_uses_active_target`",
        "Required Core rerun-038 used",
        "case1 passed and case3 reached the",
        "Route-owned supported argv checks",
        "Required Core rerun-039 used",
        "case1 created `calculator.py`, then",
        "`write.path` enum [`test_calculator.py`]",
        "`required_action`, `current_operation_template`, or `submitted_targets`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_shared_write_publiccommand_wrongtarget_exact_drift = [
        "P1-E write primitive reread",
        "`project_sandbox/fr22-2026-05-31-025-write-primitive-red-test.txt`",
        "`write_text_file` removed an existing target",
        "tool-level write/apply_patch results",
        "P0-I follow-up",
        "`subprocess.run(...)` calls",
        "Generic `spawnSync` / `child_process` tests",
        "Python-specific natural-language guidance",
        "Required Core rerun-027",
        "Free-path `write` did not recur",
        "`calculator.py` was created and `test_calculator.py`",
        "provider repeatedly submitted `apply_patch`",
        "next tool surface is constrained to `write`",
        "absolute sandbox path",
        "`inactive_target_edit_recovery_reminder_uses_current_edit_surface`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_content_shape_replay_fixture_exact_authority_drift = [
        "`required_write_content_shape_mismatch_progress_class_fixture_passes`",
        "`content_shape_mismatch_feedback_projects_current_action_fixture_passes`",
        "`required_write_content_shape_typed_progress_class`",
        "`project_sandbox/fr22-2026-06-01-003-content-shape-envelope-kind-red-preflight.json`",
        "`operation_feedback_kind` maps",
        "`lifecycle_guard_snapshot_hydration_uses_canonical_item_order_fixture_passes`",
        "`corrective_content_shape_guard_rejects_untyped_no_progress_fixture_passes`",
        "`provider_replay_result_is_supporting_context`",
        "an out-of-surface `read` ToolOutput",
        "`provider_surface_filter_requires_typed_supporting_context_signal_fixture_passes`",
        "fake `[tool feedback]` result text",
        "`provider_surface_filter_rejects_spoofed_tool_feedback_text_fixture_passes`",
        "`ModelMessage::Tool.metadata` is now `skip_serializing`",
        "`ModelMessage::Tool.metadata` is now `skip_serializing` and `skip_deserializing`",
        "plain `provider_ignored_edit_only_surface` prose",
        "`provider_surface_filter_requires_typed_provider_noncompliance_signal_fixture_passes`",
        "`prompt_assets_fixtures_are_workflow_neutral`",
        "component/widget prompt fixture authority",
        "`prompt_fixtures_are_workflow_neutral`",
        "`prompt_residual_fixtures_are_workflow_neutral`",
        "`component.py`, `test_component.py`, `python -m unittest`",
        "`python tool.py 2 + 3`",
        "`public_command_contract_fixtures_are_workflow_neutral`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_loop_state_fixture_marker_exact_authority_drift = [
        "`repair_lane_source_target_matching_rejects_sibling_suffix`",
        "`repair_lane_public_state_obligations_are_domain_neutral`",
        "`public_state_game_loop_operation_obligations`",
        "projectile/bullet/spawn/shoot/fire/collision",
        "`state_public_class_attribute_cluster_uses_single_source_coordinate`",
        "`src/workflow.ts` at the evidence target/source level",
        "`compaction_fixtures_use_sequence_order_and_workflow_neutral_targets`",
        "`Targets: component.py`",
        "`content_shape_contract_fixtures_are_workflow_neutral`",
        "Python unittest command examples",
        "`contract_reconciliation_preserves_workspace_relative_target_identity`",
        "`lifecycle_kernel_fixtures_are_workflow_neutral`",
        "Python-style `print(1)` / `print(2)` payload content",
        "`provider_surface_filter_omits_mixed_stale_assistant_prelude_fixture_passes`",
        "`loop_impl_lifecycle_guard_hydration_uses_canonical_item_order`",
        "`lifecycle_guard_snapshot_hydration_sequence_order_resists_timestamp_drift_fixture_passes`",
        "`loop_impl_control_envelope_uses_current_turn_id`",
        "`control_envelope_preserves_current_turn_id_fixture_passes`",
        "`source_content_shape_normalizes_escaped_repair_candidate_fixture_passes`",
        "Python-shaped source payload content",
        "`loop_impl_escaped_source_fixture_is_language_neutral`",
        "`loop_impl_terminal_guard_fixture_language_neutral`",
        "`loop_impl_operation_intent_fixture_language_neutral`",
        "`loop_impl_invalid_edit_fixture_language_neutral`",
        "`loop_impl_malformed_write_fixture_language_neutral`",
        "`loop_impl_malformed_apply_patch_fixture_language_neutral`",
        "`loop_impl_singleton_write_argument_fixture_language_neutral`",
        "`backend/app/main.py`, `source.py`, or `test_source.py`",
        "`loop_impl_docs_budget_edit_surface_fixture_language_neutral`",
        "`loop_impl_active_authoring_docs_regression_fixture_domain_neutral`",
        "`docs/other-workflow.md`",
        "`loop_impl_docs_existing_target_grounding_fixture_domain_neutral`",
        "Required Vision / arcade image",
        "`prompt_projection_fixture_domain_neutral`",
        "`prompt_docs_followup_heuristic_domain_neutral`",
        "`prompt_assets_python_context_uses_language_evidence_adapter`",
        "`repair_lane_typed_target_projection_no_required_action_shim`",
        "`tool_orchestrator_target_matching_exact_path_authority`",
        "`loop_impl_provider_replay_effective_surface_fixture_effective_test_payload`",
        "`prompt_provider_replay_inactive_filechange_exact_target_identity`",
        "`state_handoff_remaining_exact_target_identity`",
        "`generated_test_local_binding_enrichment_exact_target_identity`",
        "`state_docs_closeout_continuation_exact_target_identity`",
        "`state_docs_route_fixture_workflow_neutral`",
        "`state_new_authoring_turn_fixture_invariant_workspace_key`",
        "`state_generated_test_exception_overreach_fixture_domain_neutral`",
        "`canonical_docling_source_target_identity`",
        "`repair_required_active_work_without_edit_surface`",
        "`verification_dotted_technology_token_not_file_target`",
        "`app_resume_latest_user_sequence_primary_order`",
        "`cli_renderer_fixture_current_provider_profile`",
        "`llm_contract_fixture_current_provider_profile`",
        "`protocol_runtime_fixture_current_provider_profile`",
        "localhost/example-model compatibility data",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_tail_provider_registry_marker_exact_authority_drift = [
        "`protocol_store_latest_turn_position_unified_item_stream`",
        "`project_sandbox_FR22_artifact_ids`",
        "`latest_entry_parity`",
        "`source_reread_artifact_ref_set_parity`",
        "`preflight_default_fixture_required_refs_are_unique`",
        "`protocol_mod_projection_fixture_current_provider_profile`",
        "`session_markdown_legacy_toolcall_display_arguments_not_typed_projection`",
        "`session_service_fixture_current_provider_profile`",
        "`todo_completion_kind_only_open_work_authority`",
        "`session_transcript_fixture_current_provider_profile`",
        "`run_event_projection_observer_absence_not_control_plane_failure`",
        "`runtime_event_publisher_observer_absence_best_effort`",
        "`harness_recorder_protocol_first_sink_composition`",
        "`native_harness_recorder_harness_only_protocol_sink_first`",
        "`manual_st_closeout_exact_target_identity`",
        "`manual_st_generic_verification_command_contract`",
        "`provider_stream_retry_exhausted_timeout_owner`",
        "`manual_st_closeout_route_fixture_workflow_neutral_current_profile`",
        "`stored_artifact_classifier_fixture_language_neutral`",
        "`harness_replay_report_latest_run_lifecycle_order`",
        "`preflight_diagnostics_match_fixture_owner_authority`",
        "`full_access_configured_boundary_authority`",
        "`tui_query_fixture_current_provider_profile`",
        "`cli_entrypoint_artifact_atomic_commit`",
        "`staged_docs_output_exact_target_identity`",
        "`compaction_fixture_current_provider_profile`",
        "`docs_semantic_exit_code_evidence_language_neutral`",
        "`prompt_text_io_guidance_language_neutral`",
        "`public_command_source_match_exact_target_identity`",
        "`state_fixture_current_provider_profile`",
        "`state_docs_route_stale_target_workflow_neutral`",
        "`state_requested_work_diagnostic_fixture_workflow_neutral`",
        "`state_verification_diagnostic_label_fixture_workflow_neutral`",
        "`state_generated_test_repair_label_fixture_workflow_neutral`",
        "`contract_reconciliation_typed_evidence_marker_authority`",
        "`public_command_feedback_template_language_adapter_projection`",
        "`prompt_provider_replay_fixture_current_provider_profile`",
        "`prompt_artifact_target_kind_fixture_workflow_neutral`",
        "`prompt_content_shape_adapter_fixture_workflow_neutral`",
        "`prompt_workspace_root_fixture_invariant_key`",
        "`loop_terminal_accounting_fixture_current_provider_profile`",
        "`loop_request_diagnostics_fixture_current_provider_profile`",
        "`loop_request_diagnostics_parallel_fixture_current_provider_profile`",
        "`loop_consumed_image_request_diagnostics_fixture_current_provider_profile`",
        "`loop_language_neutral_fixture_helper_invariant_key`",
        "`loop_repair_grounding_fixture_language_neutral_failure_labels`",
        "`loop_runtime_owned_verification_fixture_language_neutral_labels`",
        "`loop_source_owned_repair_fixture_language_neutral_labels`",
        "`loop_control_envelope_projection_fixture_current_provider_profile`",
        "`loop_provider_replay_fixture_language_neutral_command_labels`",
        "`lifecycle_kernel_fixture_current_provider_profile`",
        "`language_content_shape_fixture_helpers_active_aggregate`",
        "`state_structured_document_summary_generated_dependency_exclusion`",
        "`desktop_markdown_export_atomic_commit`",
        "`desktop_query_todo_status_typed_projection`",
        "`desktop_web_access_mode_typed_projection`",
        "`desktop_startup_fixture_current_provider_profile`",
        "`desktop_transcript_row_kind_typed_projection`",
        "`desktop_preferences_atomic_commit`",
        "`app_initial_turn_route_key_projection`",
        "`app_default_desktop_workspace_creation_error_propagation`",
        "`protocol_store_single_item_append_order_atomic_commit`",
        "`desktop_gui_typed_visibility_projection`",
        "`manual_st_route_preflight_report_codex_style_admission`",
        "`streaming_tool_call_late_name_typed_identity`",
        "`mcp_tools_list_descriptor_schema_validation`",
        "`artifact_replay_route_evidence_content_schema`",
        "`manual_st_reference_export_scope_hygiene`",
        "`testing_metadata_current_guard_index`",
        "`preflight_gate_suite_docs_fixture_workflow_neutral`",
        "`failure_registry_header_current_entry_schema`",
        "`failure_registry_pending_status_verified_evidence_consistency`",
        "`failure_registry_verified_status_pending_plan_consistency`",
        "`failure_registry_verified_status_future_action_plan_consistency`",
        "`failure_registry_regression_fixture_authority_workflow_neutral`",
        "`failure_registry_rerun_exposed_status_verified_lifecycle`",
        "`failure_registry_verified_status_exposed_id_next_failure_consistency`",
        "`failure_registry_verified_pending_status_blocker_resolution`",
        "`failure_registry_pending_fresh_rerun_status_successor_evidence_consistency`",
        "`failure_registry_post_fix_verified_status_successor_projection_consistency`",
        "`cli_human_renderer_typed_lifecycle_projection`",
        "`NativeHarnessRecorder` no longer creates",
        "`FullAccess` keeps",
        "`ToolCallPart`",
        "`SessionRecord` metadata",
        "`ProviderModelInfo`, `ModelAvailabilityReport`",
        "`fix_verified_pending_fresh_rerun`",
        "`root_fix_verified_pending_fresh_rerun`",
        "`root_fix_verified_active_preflight_model_gate_green_pending_fresh_rerun`",
        "`self._run_cli`, `self.assertEqual`, or `self.assertTrue`",
        "`sys.exit(...)`",
    ]
    .into_iter()
    .any(|stale_phrase| content.contains(stale_phrase));
    let stale_transcript_contract_product_authority =
        content.contains("Therefore the generic `moyai` contract is:");
    let stale_stop_hook_product_authority =
        content.contains("`moyai` control-plane/state reducer defect");
    let docs_route_recovery_surface_section = content
        .split_once(
            "## 47. FR10-2026-05-09-028 Docs-Only Route Must Not Degrade To Filename Authoring",
        )
        .and_then(|(_, rest)| rest.split_once("## 56.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_docs_route_recovery_surface_exact_authority = [
        "overview/basic-design/detailed-design artifact roles",
        "detailed-design artifact role",
        "Detailed-design data-model coverage accepts",
        "`todowrite` plan-only churn",
        "`case5` gate",
        "FR10-028's root fix successfully moved the docs route",
        "model reads many source/config/data files",
        "provider stream idle timeout terminalizes the route",
        "case-level provider idle classification",
        "three distinct read/list outputs",
        "fail_turn(session_failed)",
        "corrective ToolOutput",
        "ToolLifecycleRuntime::complete_corrective_call",
        "provider stream failures",
        "repeated `docs_supporting_context_budget_exhausted` outputs",
        "provider-visible recovery surface",
        "Writing one overview or basic-design docs",
        "Reopening broad support/context actions after each",
        "provider-history growth failure class",
        "any file change to any route deliverable",
        "`content_changing_progress`",
        "file-changing tool item",
        "wrong_authoring_target metadata",
        "read/list outputs",
        "provider-visible surface",
    ]
    .into_iter()
    .any(|stale_phrase| docs_route_recovery_surface_section.contains(stale_phrase));
    let verification_patch_reference_environment_section = content
        .split_once(
            "## 57. FR10-2026-05-17-004 Verification Repair Edit Surface Is Dispatch Authority",
        )
        .and_then(|(_, rest)| rest.split_once("### 71.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_verification_patch_reference_environment_exact_authority = [
        "source-owned verification failure",
        "source repair",
        "source-owned failure targets",
        "exact source target",
        "exact source artifact-role target",
        "exact verification",
        "source/test completion",
        "source/test deliverable requests",
        "provider-visible surface",
        "generated-test artifact roles",
        "exact verification rerun authority",
        "provider-visible surface narrows",
        "`RepairOperationTemplate` / `RepairControlSnapshot`",
        "ProcessPhase::Repair",
        "diagnostic targets contain both source and generated-test files",
        "`preflight.state_reducer.requested_work_completion_promotes_verification`",
        "`preflight.state_reducer.verification_failure_preserves_repair_target_authority`",
        "ImportError cannot import name X from module (.../module.py)",
        "module.py",
        "failing test file",
        "provider response",
        "tool_choice = none",
        "SessionStatus::Completed",
        "`TurnRuntime` must cap closeout-ready final-only provider requests",
        "`PatchParser::parse()`",
        "provider-boundary transport failure",
        "`content_changing_progress`",
        "ProcessPhase::Verify with exact verification-command action authority",
        "ActionAuthority verification-command:<exact command>",
        "reference documentation artifact の仕様に合わせて implementation/test を更新",
        "previous design document reference をもとに design document only update",
        "Route08",
        "Docling",
        "`moyAI`'s Windows shell bootstrap",
        "UTF-8 process I/O settings",
        "localized text through the platform default code page",
    ]
    .into_iter()
    .any(|stale_phrase| verification_patch_reference_environment_section.contains(stale_phrase));
    let public_command_diagnostic_repair_section = content
        .split_once("### 71. FR10-2026-05-20-004: docs/spec writes need semantic reconciliation")
        .and_then(|(_, rest)| rest.split_once("### 88.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_public_command_diagnostic_repair_exact_authority = [
        "`TurnRuntime` calls",
        "`PromptAssets` projects",
        "`preflight.docs_spec.semantic_reconciliation_before_handoff`",
        "`sys.exit(N)`",
        "`未定義の関数`",
        "Manual ST specs may declare",
        "`requirement_id=public_command_contract`",
        "call-id-scoped ToolResult surfaces",
        "provider-visible continuation",
        "exact edit and rerun instructions",
        "`preflight.verification.public_command_contract_coverage`",
        "source-owned repair while the exact target",
        "`RepairOperationTemplate`",
        "`RepairControlSnapshot`",
        "exact target = source refs",
        "exact verification rerun",
        "provider still receives broad tools",
        "provider-visible surface",
        "Required action",
        "ToolOutput progress_effect=made_progress",
        "exact verification-command rerun becomes provider-visible",
        "source-owned target authority",
        "absolute Windows paths",
        "Windows or slash separators",
        "Windows path adapter",
        "Windows platform-default",
        "ToolOutput carries `encoding_contract_issues`",
        "workflow-neutral source/generated-test artifact roles",
        "exact verification-command rerun proposals",
        "ActiveWorkContract::Verification",
        "`preflight.state_reducer.post_repair_edit_promotes_verification_rerun`",
        "FunctionCallOutput carries invalid_edit_arguments + no_progress",
        "manual ST runner",
        "exact verification shell attempts",
        "corrective `ToolOutput`",
        "result_hash",
        "tool_feedback_envelope",
    ]
    .into_iter()
    .any(|stale_phrase| public_command_diagnostic_repair_section.contains(stale_phrase));
    let owner_activework_transcript_section = content
        .split_once(
            "### 88. FR10-2026-05-21-016: requested-work FileChange progress must clear authoring targets before verification",
        )
        .and_then(|(_, rest)| rest.split_once("### 105.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_owner_activework_transcript_exact_authority = [
        "ToolOutput metadata",
        "generated-test API misuse",
        "source-owned repair lane",
        "source repair was active",
        "SourceViolatesContract",
        "ToolOutput already carried",
        "Codex comparison:",
        "GUI transcript Markdown export",
        "raw protocol items",
        "The Codex reference export artifact",
        "2026-05-23 FR10-023",
        "UserTurnStored",
        "RuntimeEventMsg::TurnStarted",
        "RuntimeEventMsg::TurnCompleted",
        "UserMessageStored",
        "source-owned repair target",
        "`RepairOperationTemplate`",
        "`RepairControlSnapshot`",
        "side-effect-free wrong_repair_target ToolResult",
        "ActiveWorkContract::Verification",
        "ToolResult active target",
        "exact target",
        "source-owned verification repair",
        "source-owned generated-test evidence",
        "provider-visible active contract",
        "source-owned active-work",
        "provider-visible active contract",
        "RepairOperationTemplate / RepairControlSnapshot",
        "ActiveWorkContract / TurnControlEnvelope.active_contract",
        "accepted content-changing edit to the generated test artifact",
        "process_phase = verify",
        "stale failure.kind = verification_failed",
        "RepairLaneProjection = fail_closed / unknown target",
        "exact verification item",
        "fresh source-owned repair item",
        "RepairLaneProjection.required_target",
        "provider stream idle",
        "operation_template.exact_target",
        "RepairControlSnapshot.required_target",
        "FunctionCallOutput(success=false)",
        "exact verification pending",
        "Wrong-command ToolResult feedback",
        "FR10-010 GUI rerun",
        "post-fix CLI rerun",
        "post-FR10-024 GUI rerun",
        "representative-route id",
    ]
    .into_iter()
    .any(|stale_phrase| owner_activework_transcript_section.contains(stale_phrase));
    let semantic_command_path_transport_section = content
        .split_once(
            "### 105. FR10-2026-05-22-013: repeated semantic verification failures terminalize before provider wait",
        )
        .and_then(|(_, rest)| rest.split_once("### 113.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_semantic_command_path_transport_exact_authority = [
        "post-fix CLI rerun",
        "source-owned exact",
        "production source artifact",
        "content-changing source rewrites",
        "exact verification rerun",
        "source/test refs",
        "Codex-style root contract",
        "helper-composed concrete calls",
        "`preflight.verification.public_command_contract_coverage`",
        "deliverables = source + documentation + generated-test artifact roles",
        "ToolOutput.verification_run",
        "satisfies_command_identities=[required command identity]",
        "VerificationRunResult.failed",
        "source_refs",
        "test_refs",
        "RepairOperationTemplate",
        "source repair",
        "source-owned",
        "HistoryItem::FileChange",
        "ToolOutput.changed_files",
        "ToolCall",
        "process_phase",
        "provider-visible tools",
        "stream.next()",
        "representative-route id",
        "RepairOperationTemplate.exact_target",
        "exact target grounding",
        "exact repair target",
    ]
    .into_iter()
    .any(|stale_phrase| semantic_command_path_transport_section.contains(stale_phrase));
    let activework_stop_hook_command_target_section = content
        .split_once(
            "### 113. FR10-2026-05-22-021: implementation repair target authority is projected through active work",
        )
        .and_then(|(_, rest)| rest.split_once("## 54.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_activework_stop_hook_command_target_exact_authority = [
        "source-owned verification repair",
        "RepairOperationTemplate.exact_target",
        "FailureState.targets",
        "source-owned repair target",
        "source-owned public behavior",
        "source target",
        "ActiveWorkContract::Verification.targets",
        "ToolResult feedback",
        "source-owned repair",
        "post-FR10-022 CLI rerun",
        "provider then idled",
        "test_refs",
        "source_refs",
        "supporting read ToolOutput feedback",
        "provider-visible no-progress ToolOutput feedback",
        "ActiveWorkContract",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "exact repair target",
        "exact verification rerun",
        "latest_typed_verification_failure_context",
        "FailureKind::VerificationFailed",
        "exact repair targets",
        "verification_repair_continuation_projects_repair_state_fixture_passes",
        "workflow-neutral source/test/document artifact inventory",
        "side-effect-free corrective ToolOutput",
        "source artifact is correct",
        "source/test reconciliation",
        "preflight.verification.public_command_contract_coverage",
        "ActiveWorkContract::Verification.commands",
        "ToolResult",
        "corrective ToolOutput",
        "preflight.tool_lifecycle.verification_stable_tool_surface",
        "provider-supplied diagnostic arguments",
        "submitted ToolCall",
        "ActiveWorkContract::Verification",
        "source_parse_defect",
        "mutable source frame",
        "source ref",
        "RepairControlSnapshot.required_target",
        "ToolResult feedback",
        "source-owned parse defects",
        "source-owned when evidence target/source refs",
        "repair_write_arguments_from_active_target",
        "ManualStCloseoutEvidence.repair_targets",
        "source repair continuation",
        "source artifact",
        "build_verification_repair_continuation_prompt",
    ]
    .into_iter()
    .any(|stale_phrase| activework_stop_hook_command_target_section.contains(stale_phrase));
    let stale_rerun_recovery_required_action_section = content
        .split_once("## 54. FR10-2026-05-24-034: stop-hook typed evidence feeds repair lane")
        .and_then(|(_, rest)| rest.split_once("## 75.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_rerun_recovery_required_action_exact_authority = [
        "source-owned",
        "source refs",
        "test refs",
        "ActiveWorkContract",
        "RepairControlSnapshot",
        "RepairOperationTemplate",
        "ToolOutput",
        "ToolResult",
        "ToolCall",
        "ToolLifecycleRuntime",
        "ProjectionSurface",
        "ActionAuthority",
        "provider-visible",
        "provider schema-adherence",
        "strict transport hint",
        "provider tool-choice",
        "provider dispatch",
        "ProcessPhase::Repair",
        "ProcessPhase::Verify",
        "typed required action projection",
        "source/test",
        "case1",
        "calculator",
        "subprocess",
        "unittest",
        "source artifact",
        "source-authoring",
        "write:<active target>",
        "inspect_directory",
        "apply_patch` / `write",
        "filter_provider_messages_to_effective_tool_surface",
        "provider_replay_effective_tool_surface_fixture_passes",
        "SourceTestContractMismatch",
        "TestViolatesContract",
        "source_repair_allowed=true",
        "test_repair_allowed=true",
        "exact repair surface",
        "exact required action",
        "exact singleton recovery",
        "fake target marker",
    ]
    .into_iter()
    .any(|stale_phrase| stale_rerun_recovery_required_action_section.contains(stale_phrase));
    let content_shape_runtime_docs_recovery_section = content
        .split_once("## 75. FR10-2026-05-24-055: implementation public output repair drives active target authority")
        .and_then(|(_, rest)| rest.split_once("## 95.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_content_shape_runtime_docs_recovery_exact_authority = [
        "source-owned",
        "source repair",
        "source refs",
        "source_refs",
        "source/test",
        "generated-test-owned",
        "exact repair target",
        "exact active",
        "exact target",
        "exact shell",
        "exact required shell command",
        "exact runtime-owned verification command",
        "exact write",
        "exact action",
        "exact docs write",
        "source artifact",
        "source artifacts",
        "source parse",
        "source edit",
        "production source",
        "provider-visible",
        "provider-compliance",
        "provider to submit",
        "provider drift",
        "provider schema",
        "provider selection",
        "provider surface",
        "provider replay",
        "Windows absolute path",
        "Windows path",
        "ToolLifecycleEnvelope",
        "ToolLifecycleRuntime",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "FileChange",
        "TurnControlEnvelope",
        "ActionAuthority",
        "ActiveWorkContract",
        "SessionStateSnapshot",
        "TurnObligation",
        "ProcessPhase::Author",
        "HistoryItem",
        "FunctionCallOutput-equivalent",
        "LanguageEvidenceAdapter",
        "PublicCommandContract",
        "Contract Reconciliation",
        "Repair lane",
        "read(calculator.py)",
        "route2",
        "`write` / `apply_patch`",
        "`apply_patch` / `write`",
        "`read`",
        "`shell`",
        "`todowrite`",
        "`tool_choice=auto`",
        "allowed_tools=[shell]",
        "typed required action projection",
        "write:<target>",
        "apply_patch:<target>",
        "shell:<command>",
        "write:<active target>",
        "invalid_edit_arguments",
        "wrong_authoring_target",
        "operation_progress_class=authoring_target_grounding_required",
        "MalformedEditArguments",
        "test_target_content_shape_violation_result",
        "post_patch_test_module_shape_contract",
        "preflight.tool_lifecycle.active_authoring_rejects_wrong_target",
        "preflight.turn_decision.codex_stable_tool_surface_authority",
        "text_artifact_readable_content_shape",
        "serialized_markdown_rejected",
        "content_shape_workspace_target_normalization",
        "supporting_context_evidence_survives_surface_narrowing",
        "docs_content_grounding_before_exact_write_recovery",
    ]
    .into_iter()
    .any(|stale_phrase| content_shape_runtime_docs_recovery_section.contains(stale_phrase));
    let recovery_toolchoice_grounding_section = content
        .split_once("## 95. FR10-2026-05-24-075: recovery surfaces recompile from stable tools")
        .and_then(|(_, rest)| rest.split_once("## 109.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_recovery_toolchoice_grounding_exact_authority = [
        "source-owned",
        "source repair",
        "source target",
        "source FileChange",
        "source/test",
        "exact action",
        "exact edit",
        "exact write",
        "exact read",
        "exact shell",
        "exact file-changing authority",
        "exact source edit",
        "exact-target",
        "write:<target>",
        "write:<test target>",
        "ToolResult",
        "ToolOutput",
        "HistoryItem",
        "TurnControlEnvelope",
        "ActionAuthority",
        "ActiveWorkContract",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "FileChange",
        "Required action",
        "provider-visible",
        "provider-side",
        "provider request",
        "provider dispatch",
        "provider receives",
        "provider tool selection",
        "required provider tool selection",
        "provider-portable required tool selection",
        "local provider",
        "provider copies",
        "provider-ignored",
        "provider schema",
        "provider tool choice",
        "provider tool_choice",
        "generated-test artifact",
        "generated-test target",
        "generated-test authoring",
        "source-reference",
        "calculator",
        "manual ST",
        "docs_route_flat_test_artifact_satisfies_required_area_fixture_passes",
        "consumed_supporting_context_pair_omitted",
        "text_artifact_repair_positive_contract",
        "source_repair_positive_contract",
        "final_dispatch_source_schema_projection",
        "normal_authoring_final_message_recovery_stable_surface",
        "forbidden_supporting_tool_before_repair_edit",
        "authoring_final_message_target_grounding_read",
        "generated_test_source_reference_grounding_after_source_change",
        "verification_target_grounding_active",
    ]
    .into_iter()
    .any(|stale_phrase| recovery_toolchoice_grounding_section.contains(stale_phrase));
    let consumed_reference_regrounding_section = content
        .split_once("## 109. FR10-2026-05-24-091: consumed source reference does not reopen non-active reads")
        .and_then(|(_, rest)| rest.split_once("## 117.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_consumed_reference_regrounding_exact_authority = [
        "source reference",
        "source-reference",
        "source FileChange",
        "source read",
        "source-owned",
        "source repair",
        "source target",
        "source write",
        "source string",
        "source executable",
        "source/test",
        "generated-test",
        "test target",
        "production source",
        "exact target",
        "exact-target",
        "exact source",
        "exact edit",
        "exact write",
        "exact read",
        "exact docs",
        "exact missing singleton",
        "required action",
        "Required action",
        "provider-visible",
        "provider replay",
        "provider selection",
        "provider-selection",
        "provider tool selection",
        "provider submits",
        "ToolResult",
        "ToolOutput",
        "TurnControlEnvelope",
        "ActionAuthority",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "ActiveWorkContract",
        "FileChange",
        "CandidateRepairEdit",
        "ToolLifecycleEnvelope",
        "active_targets",
        "grounded_targets",
        "consumed_targets",
        "missing_grounding_targets",
        "satisfying_actions",
        "serialized_source_write_candidate_normalized",
        "generated_test_source_reference_consumed_target_grounding_active",
        "generated_test_consumed_source_reference_active_target_grounding",
        "generated_test_target_grounding_required",
        "docs_open_obligation_required_edit_recovery",
        "case1",
        "calculator",
        "manual ST",
        "workflow-neutral source/test",
    ]
    .into_iter()
    .any(|stale_phrase| consumed_reference_regrounding_section.contains(stale_phrase));
    let provider_surface_stable_recovery_section = content
        .split_once("## 117. FR10-2026-05-24-099: superseded tool-turn output budget policy")
        .and_then(|(_, rest)| rest.split_once("## 131.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_provider_surface_stable_recovery_exact_authority = [
        "provider",
        "tool_choice",
        "tool-choice",
        "ToolResult",
        "ToolOutput",
        "TurnControlEnvelope",
        "ActionAuthority",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "ActiveWorkContract",
        "VerificationFailureCluster",
        "SessionStateSnapshot",
        "DocsRouteState",
        "DocsRepair",
        "RequestedWorkAuthoring",
        "FileChange",
        "PromptProjection",
        "FunctionCall",
        "ProviderActionAdapter",
        "TurnLifecycleKernel",
        "ToolLifecycleRuntime",
        "RejectedModelAction",
        "RejectedToolProposal",
        "MalformedToolArguments",
        "ModelConfig.max_output_tokens",
        "configured_tool_turn_output_budget",
        "tool_call_turn_output_budget",
        "open_obligation_final_message_recovery_tool_visible",
        "open_obligation_final_message_recovery_tool_choice",
        "normal_authoring_final_message_recovery_stable_surface",
        "preflight.",
        "source-owned",
        "source/test",
        "source reference",
        "source-reference",
        "source write",
        "source read",
        "source artifact",
        "generated-test",
        "generated test",
        "test target",
        "exact write",
        "exact edit",
        "exact target",
        "exact verification",
        "whole-file JSON",
        "whole-file edit",
        "whole-file write",
        "write(path, content)",
        "write:<target>",
        "read/list",
        "apply_patch",
        "todowrite",
        "shell",
        "route/domain",
        "domain labels",
        "route labels",
        "provider-profile",
        "case1",
        "calculator",
        "unittest",
        "python",
        "widget.py",
        "test_widget.py",
        "source.py",
        "test_source.py",
        "manual ST",
    ]
    .into_iter()
    .any(|stale_phrase| provider_surface_stable_recovery_section.contains(stale_phrase));
    let no_progress_control_projection_section = content
        .split_once("## 131. FR20-2026-05-25-024: invalid edit no-progress cannot satisfy requested-work authoring")
        .and_then(|(_, rest)| rest.split_once("## 144.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_no_progress_control_projection_exact_authority = [
        "provider",
        "tool_choice",
        "tool-choice",
        "provider-visible",
        "provider request",
        "provider response",
        "provider selection",
        "ToolResult",
        "ToolOutput",
        "TurnControlEnvelope",
        "ActionAuthority",
        "ActiveWorkContract",
        "DocsRepair",
        "DocsRouteState",
        "TaskRoute::Docs",
        "TaskRoute::Code",
        "ProcessPhase::Author",
        "ProcessPhase::Repair",
        "ContentChangingAuthoringRequired",
        "FileChange",
        "HistoryItem::FileChange",
        "PatchParser",
        "PatchParser::normalize_patch_text",
        "repair_patch_structure",
        "ModelMessage::User",
        "RequestMessageDiagnostic",
        "InvalidEditRecoveryEnvelope",
        "Option<String>",
        "provider_messages_for_dispatch_control",
        "ToolLifecycleRuntime",
        "TurnRuntime",
        "Add File",
        "Update File",
        "apply_patch",
        "write",
        "read",
        "grep",
        "shell",
        "todo",
        "todowrite",
        "exact",
        "source/test",
        "source artifact",
        "source deliverable",
        "generated-test",
        "generated test",
        "test deliverable",
        "Python",
        "case1",
        "calculator",
        "manual ST",
        "preflight.",
        "fixture",
        "000027",
        "000029",
        "Chinese",
        "OpenAI-compatible-only",
    ]
    .into_iter()
    .any(|stale_phrase| no_progress_control_projection_section.contains(stale_phrase));
    let recovery_route_control_section = content
        .split_once("## 144. FR20-2026-05-26-009: final-message recovery persists across no-progress supporting tools")
        .and_then(|(_, rest)| rest.split_once("## 173.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_recovery_route_control_exact_authority = [
        "provider",
        "tool_choice",
        "tool-choice",
        "provider-visible",
        "provider selection",
        "provider-selection",
        "provider wording",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "TurnControlEnvelope",
        "TurnLifecycleKernel",
        "TurnLifecycleRecoveryContext",
        "ToolLifecycleRuntime",
        "ManualStCloseoutEvidence",
        "VerificationCommandEvidence",
        "RunService::execute",
        "ProjectionBundle",
        "RepairOperationTemplate",
        "VerificationFailureCluster",
        "SourceViolatesContract",
        "TestViolatesContract",
        "ContractInsufficient",
        "FileChange",
        "ChatRequest::effective_max_output_tokens",
        "OpenObligationFinalMessageRecoveryEnvelope",
        "InvalidEditRecoveryEnvelope",
        "Option<String>",
        "apply_patch",
        "write",
        "read",
        "shell",
        "todowrite",
        "exact",
        "source-owned",
        "source/test",
        "source artifact",
        "source target",
        "source repair",
        "generated-test",
        "generated test",
        "test target",
        "Code Author",
        "Docs Author",
        "Manual ST",
        "manual ST",
        "case",
        "calculator",
        "PowerShell",
        "python",
        "preflight.",
        "fixture",
        "raw result",
        "raw payload",
        "raw patch",
        "domain filenames",
        "route label",
        "route-owned",
        "000012",
        "000014",
        "000019",
        "000024",
    ]
    .into_iter()
    .any(|stale_phrase| recovery_route_control_section.contains(stale_phrase));
    let semantic_recovery_command_grounding_section = content
        .split_once("## 173. FR21-2026-05-27-014: docs semantic corrective output is projected as recovery obligation")
        .and_then(|(_, rest)| rest.split_once("## 185.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_semantic_recovery_command_grounding_exact_authority = [
        "provider",
        "provider-visible",
        "provider selection",
        "provider-required",
        "provider-specific",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "TurnControlEnvelope",
        "TurnLifecycleKernel",
        "TurnLifecyclePlan",
        "TurnLifecycleRecoveryContext",
        "ProjectionBundle",
        "DocsSpecSemanticContract",
        "InvalidEditRecoveryEnvelope",
        "SemanticClaimProjection",
        "FileChange",
        "apply_patch",
        "write",
        "read",
        "shell",
        "todowrite",
        "exact",
        "source-owned",
        "source/test",
        "source artifact",
        "source target",
        "source repair",
        "generated-test",
        "generated test",
        "test target",
        "Manual ST",
        "manual ST",
        "case",
        "calculator",
        "PowerShell",
        "Python",
        "raw traceback",
        "raw path",
        "raw payload",
        "preflight.",
        "fixture",
        "domain filename",
        "route-owned",
        "OpenAI-compatible",
        "named dispatch",
        "named-write",
    ]
    .into_iter()
    .any(|stale_phrase| semantic_recovery_command_grounding_section.contains(stale_phrase));
    let semantic_transport_repair_projection_section = content
        .split_once("## 185. FR21-2026-05-27-026: docs semantic repair projection carries required snippets")
        .and_then(|(_, rest)| rest.split_once("## 204.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_semantic_transport_repair_projection_exact_authority = [
        "provider",
        "provider-visible",
        "provider selection",
        "provider-required",
        "provider-specific",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "FunctionCallOutput",
        "TurnControlEnvelope",
        "TurnLifecycleKernel",
        "TurnLifecyclePlan",
        "TurnLifecycleRecoveryContext",
        "ProjectionBundle",
        "RequestDiagnosticsPart",
        "LlmError",
        "DocsSpecSemanticContract",
        "SemanticClaimProjection",
        "CloseoutContinuationBudget",
        "MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE",
        "InvalidEditRecoveryEnvelope",
        "VerificationFailureEvidence",
        "VerificationFailureCluster",
        "TestViolatesContract",
        "SourceViolatesContract",
        "Contract Reconciliation",
        "FileChange",
        "ThreadItem",
        "apply_patch",
        "write",
        "read",
        "shell",
        "todowrite",
        "exact",
        "source-owned",
        "source/test",
        "source artifact",
        "source target",
        "source repair",
        "generated-test",
        "generated test",
        "test target",
        "Manual ST",
        "manual ST",
        "case",
        "calculator",
        "Python",
        "raw",
        "traceback",
        "preflight.",
        "fixture",
        "domain",
        "route-owned",
        "route case",
        "OpenAI-compatible",
        "case_progress.json",
        "route_manifest.json",
    ]
    .into_iter()
    .any(|stale_phrase| semantic_transport_repair_projection_section.contains(stale_phrase));
    let verdict_owner_generated_artifact_section = content
        .split_once(
            "## 204. FR22-2026-05-28-006: successful continuation re-materializes route verdict",
        )
        .and_then(|(_, rest)| rest.split_once("## 213.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_verdict_owner_generated_artifact_exact_authority = [
        "provider",
        "provider-visible",
        "provider-profile",
        "provider-specific",
        "provider response",
        "provider dispatch",
        "provider boundary",
        "provider conformance",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "FunctionCallOutput",
        "TurnControlEnvelope",
        "TurnLifecycleKernel",
        "TurnLifecycleRecoveryContext",
        "RepairControlSnapshot",
        "RepairLane",
        "ContractReconciliation",
        "ActiveWorkContract",
        "ManualStCaseResult",
        "Manual ST",
        "manual ST",
        "case",
        "route-owned",
        "route_manifest.json",
        "case_progress.json",
        "source-owned",
        "source artifact",
        "source target",
        "source path",
        "source repair",
        "generated-test",
        "generated test",
        "test target",
        "test frame",
        "docs-route",
        "docs route",
        "apply_patch",
        "write",
        "read",
        "shell",
        "tool_choice",
        "tool-choice",
        "exact",
        "raw",
        "traceback",
        "preflight.",
        "fixture",
        "calculator",
        "domain",
        "000023",
        "run_case",
        "run_case",
        "materialize_manual_st_case_terminal_verdict",
        "materialize_manual_st_route_terminal_verdict",
        "CloseoutContinuationBudget",
        "RouteStageTerminalContinuationLedger",
        "VerificationFailureCluster",
        "TestViolatesContract",
        "ProviderCapabilityMismatch",
        "ToolOrEnvironmentFailure",
        "Provider Boundary Stop",
        "Implementation status",
    ]
    .into_iter()
    .any(|stale_phrase| verdict_owner_generated_artifact_section.contains(stale_phrase));
    let later_route_verification_source_shape_section = content
        .split_once(
            "## 213. FR22-2026-05-29-019: post-repair route verification is harness-owned evidence",
        )
        .and_then(|(_, rest)| rest.split_once("## 222.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_later_route_verification_source_shape_exact_authority = [
        "provider",
        "provider-visible",
        "provider request",
        "provider messages",
        "provider replay",
        "provider capability",
        "ToolLifecycleRuntime",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "FunctionCallOutput",
        "ChatRequest",
        "ModelContentPart",
        "TurnContext",
        "ActiveWorkContract",
        "ProcessPhase",
        "FailureKind",
        "VerificationFailureCluster",
        "RouteStageTerminalContinuationLedger",
        "CloseoutContinuationBudget",
        "SessionStatus::Completed",
        "Manual ST",
        "manual ST",
        "representative",
        "case",
        "route stage",
        "route label",
        "route_manifest.json",
        "case_progress.json",
        "source-owned",
        "source target",
        "source artifact",
        "source file",
        "source repair",
        "generated-test",
        "generated test",
        "test-target",
        "test target",
        "write",
        "read",
        "shell",
        "exact",
        "raw",
        "preflight.",
        "fixture",
        "Implementation status",
        "post_repair_route_verification_clears_stale_repair_fixture_passes",
        "source_content_shape_rejects_duplicate_entrypoint_fixture_passes",
        "verification_turn_omits_consumed_images_fixture_passes",
        "provider_chat_request_omits_consumed_images_fixture_passes",
        "verification_repair_terminal_ledger_blocks_non_obligation_workspace_progress_fixture_passes",
        "source_executable_shape_accepts_required_public_surface_fixture_passes",
        "source_duplicate_entrypoint_rejected",
        "consumed_vision_image_not_reattached_for_verification",
        "consumed_vision_image_not_reattached_for_verification_repair",
        "consumed_vision_image_not_reattached_in_chat_request",
        "tool_no_progress_terminal_cluster",
        "verification_non_convergence_terminal_cluster",
        "ineffective_verification_repair_progress_does_not_reset_terminal_ledger",
        "source_required_public_surface_allowed",
    ]
    .into_iter()
    .any(|stale_phrase| later_route_verification_source_shape_section.contains(stale_phrase));
    let terminal_replay_current_item_section = content
        .split_once(
            "## 222. FR22-2026-05-30-011: edit-only grounded-read terminal は route continuation を開かない",
        )
        .and_then(|(_, rest)| rest.split_once("## 232.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_terminal_replay_current_item_exact_authority = [
        "provider",
        "provider-visible",
        "provider capability",
        "provider replay",
        "local provider",
        "ToolLifecycleRuntime",
        "ToolResult",
        "ToolOutput",
        "ToolCall",
        "FunctionCallOutput",
        "TurnControlEnvelope",
        "RepairControlSnapshot",
        "PublicCommandContract",
        "PublicCommandObligation",
        "FileChange",
        "FileChangeAdmission",
        "VerificationExecutionItem",
        "ArtifactRole",
        "ActionAuthority",
        "TurnLifecyclePlan",
        "TerminalItemCluster",
        "AssistantToolCall",
        "Manual ST",
        "manual ST",
        "representative",
        "route stage",
        "route label",
        "case",
        "source-owned",
        "source target",
        "source artifact",
        "source deliverable",
        "source path",
        "source repair",
        "source-shaped",
        "source candidate",
        "generated-test",
        "generated test",
        "test target",
        "test artifact",
        "test runner",
        "test-only",
        "write",
        "read",
        "apply_patch",
        "shell",
        "whole-file",
        "exact",
        "raw",
        "path enum",
        "host-local",
        "absolute path",
        "Python",
        "JS",
        "LM Studio",
        "preflight.",
        "fixture",
        "Implementation status",
        "authoring_grounding_terminal_fail_stops_route_fixture_passes",
        "content_changing_authoring_no_progress_terminal_fail_stops_route_fixture_passes",
        "failed_inactive_executable_pair_omitted",
        "wrong_target_authoring_recovery_hardens_active_target_fixture_passes",
        "malformed_apply_patch_recovery_overrides_stale_wrong_target_fixture_passes",
        "failed_inactive_authoring_executable_pair_omitted",
        "exact_write_wrong_path_content_shape_uses_active_target",
        "content_shape_mismatch_current_action_projection",
        "generic_child_process_timeout_feedback",
        "language_source_artifact_shape_accepts_required_public_surface_fixture_passes",
        "verification_process_cleanup_fixture_passes",
        "public_command_observation_assertion_templates",
        "shared_write_commit_atomicity_fixture_passes",
    ]
    .into_iter()
    .any(|stale_phrase| terminal_replay_current_item_section.contains(stale_phrase));
    let replay_metadata_current_item_section = content
        .split_once(
            "## 232. FR22-2026-05-31-006: wrong-target reminder must not reintroduce a second edit contract",
        )
        .and_then(|(_, rest)| rest.split_once("## 242.").map(|(section, _)| section))
        .unwrap_or("");
    let stale_replay_metadata_current_item_exact_authority = [
        "provider",
        "provider-visible",
        "provider-facing",
        "provider replay",
        "provider request",
        "provider message",
        "provider transport",
        "provider serialization",
        "provider-message",
        "ToolResult",
        "ToolOutput",
        "FunctionCallOutput",
        "FileChange",
        "TurnControlEnvelope",
        "ToolFeedbackEnvelope",
        "HistoryItemPayload",
        "ModelMessage::Tool",
        "ReplayNormalizer",
        "ToolLifecycleOwner",
        "generated-test",
        "generated test",
        "source target",
        "source artifact",
        "source-reference",
        "source/test",
        "test-only",
        "write",
        "read",
        "patch-oriented",
        "whole-file",
        "tool surface",
        "tool spelling",
        "raw",
        "result-string",
        "result text",
        "result prose",
        "host-local",
        "absolute path",
        "OpenAI",
        "Manual ST",
        "manual ST",
        "fixture",
        "preflight.",
        "Implementation status",
        "wrong_target_authoring_recovery_hardens_active_target_fixture_passes",
        "progress_projection_recovery_narrows_to_edit_surface_fixture_passes",
        "malformed_apply_patch_write_recovery_surface",
        "malformed_apply_patch_recovery_overrides_stale_wrong_target_fixture_passes",
        "failed_inactive_executable_pair_omitted",
        "failed_inactive_non_executable_feedback_projected",
        "content_shape_mismatch_current_action_projection",
        "operation_progress_class=no_progress",
        "operation_progress_class",
        "tool_feedback_envelope",
        "supporting_context",
        "model_action_adjudication",
        "metadata.tool_feedback_envelope",
        "prompt_replay",
    ]
    .into_iter()
    .any(|stale_phrase| replay_metadata_current_item_section.contains(stale_phrase));
    let fixture_identity_order_section = content
        .split_once(
            "## 242. FR22-2026-06-03-087: prompt-assets fixture authority must be workflow-neutral",
        )
        .and_then(|(_, rest)| {
            rest.split_once("## FR22-2026-06-03-127")
                .map(|(section, _)| section)
        })
        .unwrap_or("");
    let stale_fixture_identity_order_exact_authority = [
        "prompt_assets.rs",
        "agent::prompt",
        "public_command_contract.rs",
        "repair_lane.rs",
        "state.rs",
        "agent::compaction",
        "agent::contract_reconciliation",
        "lifecycle_kernel.rs",
        "loop_impl.rs",
        "active_targets_contain_repair_target",
        "source_targets_equivalent",
        "VerificationFailureCluster",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "HistoryItem",
        "HistoryItem.sequence_no",
        "LifecycleGuardSnapshot",
        "LifecycleGuardState::hydrate_from_history_items",
        "compile_turn_control_envelope",
        "AgentRunRequest.protocol_turn_id",
        "TurnControlEnvelope.turn_id",
        "TurnRuntime",
        "TurnId::new",
        "source/test",
        "generated-test",
        "source-owned",
        "source refs",
        "source coordinate",
        "component",
        "widget",
        "calculator",
        "game",
        "domain",
        "manual-ST",
        "Manual ST",
        "Python",
        "TypeScript",
        "Rust",
        "src/widget.test.ts",
        "test_widget.py",
        "test_component.py",
        "component.py",
        "src/workflow.rs",
        "workflow.rs",
        "workflow source",
        "verification command",
        "command examples",
        "fixture",
        "fixtures",
        "preflight",
        "marker",
        "Implementation status",
        "path",
        "coordinate",
        "basename",
        "file_name",
        "wall-clock",
        "timestamp",
        "created_at_ms",
        "sequence_no",
    ]
    .into_iter()
    .any(|stale_phrase| fixture_identity_order_section.contains(stale_phrase));
    let tail_catalog_current_authority_section = content
        .split_once("## FR22-2026-06-03-127 Prompt Docs Follow-Up Heuristic")
        .map(|(_, section)| section)
        .unwrap_or("");
    let stale_tail_catalog_current_authority = [
        "Python-specific",
        "Python-like",
        "Python wording",
        "LanguageEvidenceAdapter",
        "RepairOperationTemplate",
        "RepairControlSnapshot",
        "ToolOutput",
        "ToolCall",
        "ToolCall / ToolOutput",
        "FileChange",
        "HistoryItem",
        "HistoryItemPayload",
        "provider",
        "Provider",
        "provider-boundary",
        "provider literal",
        "provider-profile",
        "OpenAI-compatible",
        "source/test",
        "source refs",
        "source-owned",
        "generated-test",
        "generated test",
        "test artifact",
        "test target",
        "basename",
        "suffix",
        "path tokens",
        "workspace-relative",
        "Docling",
        "shell-only",
        "CLI",
        "Desktop",
        "TUI",
        "fixture",
        "fixtures",
        "preflight",
        "executable evidence id catalog",
        "stored_artifact_classifier_fixture_language_neutral",
        "verification_command_encoding_alias",
        "moyai-new-authoring-turn",
        "FR ids",
        "manual-ST",
        "divide-by-zero",
        "Arithmetic/calculator",
        "localized",
        "Rust debug",
        "streamed tool calls",
    ]
    .into_iter()
    .any(|stale_phrase| tail_catalog_current_authority_section.contains(stale_phrase));

    has_current_target_path
        && has_current_design_target
        && has_current_authority_notice
        && !stale_target_path
        && !stale_design_target
        && !stale_current_authority_intro
        && !stale_design_rule_lifecycle_plan_owner
        && has_current_lifecycle_owner_section
        && !stale_lifecycle_kernel_section
        && !stale_lifecycle_kernel_global_phrase
        && !stale_required_tool_choice_kernel_boundary
        && !stale_source_artifact_admission_payload_authority
        && !stale_provider_noncompliance_owner_authority
        && !stale_runtime_flow_compatibility_bridge_authority
        && !stale_transcript_compatibility_authority
        && !stale_provider_replay_closeout_example_authority
        && !stale_ui_transcript_compatibility_boundary_authority
        && !stale_immediate_consequence_exact_surface_authority
        && !stale_implementation_consequence_owner_surface_authority
        && !stale_route_target_reference_exact_surface_authority
        && !stale_answer_replay_verification_surface_authority
        && !stale_feedback_toolchoice_continuation_surface_authority
        && !stale_continuation_provider_replay_surface_authority
        && !stale_progress_projection_tool_surface_authority
        && !stale_repair_target_evidence_surface_authority
        && !stale_domain_fixture_authority
        && !stale_current_product_authority
        && !stale_implementation_status_future_action
        && !stale_implementation_status_registered_before_fix
        && !stale_short_current_fr_heading
        && !stale_current_runtime_product_path_authority
        && !stale_closeout_route_layer_authority
        && !stale_consequence_product_authority
        && !stale_tooloutput_metadata_authority
        && !stale_current_authority_exact_edit_surface_recovery
        && !stale_prompt_verification_repair_exact_shell_surface
        && !stale_current_edit_operation_exact_surface
        && !stale_tool_lifecycle_runtime_exact_surface_command
        && !stale_lifecycle_kernel_redesign_exact_surface
        && !stale_shared_vocabulary_fr10_exact_surface
        && !stale_current_tool_lifecycle_single_ledger_exact_surface
        && !stale_closeout_contract_exact_surface_remaining_slices
        && !stale_operation_intent_exact_surface_domain_fixture
        && !stale_verification_route_map_exact_surface_domain
        && !stale_fr10_route_map_exact_surface_domain
        && !stale_fr10_feedback_target_projection_exact_surface_domain
        && !stale_closeout_vision_hook_exact_surface_domain
        && !stale_provider_replay_progress_exact_surface_domain
        && !stale_docs_repair_label_target_exact_surface_domain
        && !stale_recovery_repair_surface_exact_domain
        && !stale_runtime_patch_verification_reference_exact_domain
        && !stale_public_command_delta_verification_exact_surface_domain
        && !stale_repair_target_transcript_byproduct_exact_surface_domain
        && !stale_source_generated_test_active_target_exact_surface_domain
        && !stale_verification_continuation_closeout_exact_domain
        && !stale_output_path_stream_read_stop_hook_exact_domain
        && !stale_subprocess_command_repair_exact_domain
        && !stale_recovery_provider_finalmessage_path_exact_domain
        && !stale_artifact_shape_language_specific_authority_drift
        && !stale_source_test_grounding_recovery_language_surface_drift
        && !stale_multitarget_docs_recovery_exact_surface_drift
        && !stale_stable_surface_plan_sidechannel_exact_surface_drift
        && !stale_repair_activework_inactive_snapshot_exact_surface_drift
        && !stale_docs_authoring_finalmessage_exact_surface_drift
        && !stale_recovery_projection_exact_surface_drift
        && !stale_hard_recovery_system_authority_exact_surface_drift
        && !stale_grounding_content_budget_exact_surface_drift
        && !stale_wrongtarget_closeout_legacywrite_exact_surface_drift
        && !stale_progress_public_docs_provider_exact_surface_drift
        && !stale_wrongtarget_publiccommand_invalidedit_exact_drift
        && !stale_provider_generatedtest_docs_grounding_exact_drift
        && !stale_docs_closeout_command_semantic_transport_exact_drift
        && !stale_docs_patch_budget_transport_semantic_exact_drift
        && !stale_generatedtest_invalidedit_samedoc_exact_drift
        && !stale_closeout_content_shape_verdict_exact_drift
        && !stale_generated_subprocess_docsroute_source_vision_exact_drift
        && !stale_later_vision_terminal_source_shape_exact_drift
        && !stale_failed_inactive_wrongtarget_malformed_edit_exact_drift
        && !stale_shared_write_publiccommand_wrongtarget_exact_drift
        && !stale_content_shape_replay_fixture_exact_authority_drift
        && !stale_loop_state_fixture_marker_exact_authority_drift
        && !stale_tail_provider_registry_marker_exact_authority_drift
        && !stale_transcript_contract_product_authority
        && !stale_stop_hook_product_authority
        && !stale_docs_route_recovery_surface_exact_authority
        && !stale_verification_patch_reference_environment_exact_authority
        && !stale_public_command_diagnostic_repair_exact_authority
        && !stale_owner_activework_transcript_exact_authority
        && !stale_semantic_command_path_transport_exact_authority
        && !stale_activework_stop_hook_command_target_exact_authority
        && !stale_rerun_recovery_required_action_exact_authority
        && !stale_content_shape_runtime_docs_recovery_exact_authority
        && !stale_recovery_toolchoice_grounding_exact_authority
        && !stale_consumed_reference_regrounding_exact_authority
        && !stale_provider_surface_stable_recovery_exact_authority
        && !stale_no_progress_control_projection_exact_authority
        && !stale_recovery_route_control_exact_authority
        && !stale_semantic_recovery_command_grounding_exact_authority
        && !stale_semantic_transport_repair_projection_exact_authority
        && !stale_verdict_owner_generated_artifact_exact_authority
        && !stale_later_route_verification_source_shape_exact_authority
        && !stale_terminal_replay_current_item_exact_authority
        && !stale_replay_metadata_current_item_exact_authority
        && !stale_fixture_identity_order_exact_authority
        && !stale_tail_catalog_current_authority
}

pub fn failure_registry_header_current_entry_schema_fixture_passes() -> bool {
    let registry_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("docs").join("testing").join("FailureRegistry.md"));
    let Some(registry_path) = registry_path else {
        return false;
    };
    let Ok(content) = fs::read_to_string(registry_path) else {
        return false;
    };
    [
        "representative NG, route NG, preflight NG, timeout, provider error, harness error, system/e2e failure, and regression/preflight failures",
        "`deeper_root_cause`",
        "`codex_aligned_deeper_root_fix`",
        "`provider_boundary_assessment`",
        "`harness_boundary_assessment`",
    ]
    .into_iter()
    .all(|required| content.contains(required))
}

pub fn failure_registry_markdown_json_status_parity_fixture_passes() -> bool {
    fn markdown_entries(markdown: &str) -> Vec<(String, String)> {
        markdown
            .split("\n## ")
            .skip(1)
            .filter_map(|block| {
                let id = block.lines().next()?.trim().to_string();
                let status = block
                    .lines()
                    .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                    .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                    .unwrap_or_default()
                    .to_string();
                Some((id, status))
            })
            .collect()
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };
    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };

    let markdown_entries = markdown_entries(&markdown);
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((id.to_string(), status.to_string()))
        })
        .collect::<Vec<_>>();

    markdown_entries == json_entries
}

pub fn failure_registry_pending_status_verified_evidence_consistent_fixture_passes() -> bool {
    fn pending_lower_tier_status(status: &str) -> bool {
        status.contains("pending_lower_tier_reproduction")
    }

    fn verified_regression_evidence(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        let has_green = lower.contains("green")
            || lower.contains("implemented_and_green")
            || lower.contains("implemented.");
        let has_regression_authority = lower.contains("covered by active preflight")
            || lower.contains("active cli preflight")
            || lower.contains("codex_style_preflight")
            || lower.contains("cargo check")
            || lower.contains("cargo test");
        has_green && has_regression_authority
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        if pending_lower_tier_status(status_line) && verified_regression_evidence(block) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !pending_lower_tier_status(status) {
            continue;
        }
        let regression = entry
            .get("regression_test")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if verified_regression_evidence(regression) {
            return false;
        }
    }

    true
}

pub fn failure_registry_implemented_pending_status_verified_evidence_consistent_fixture_passes()
-> bool {
    fn verified_evidence_text(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        lower.contains("verified ")
            || lower.contains("verified `")
            || lower.contains(" passed")
            || lower.contains("active preflight")
            || lower.contains("model gate")
            || lower.contains("cargo check")
            || lower.contains("cargo test")
    }

    fn projection_is_consistent(entries: &[(String, String)]) -> bool {
        entries.iter().all(|(status, text)| {
            status != "root_fix_implemented_pending_verification" || !verified_evidence_text(text)
        })
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    if !projection_is_consistent(&markdown_entries) {
        return false;
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let direct_cause = entry
                .get("direct_cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let root_cause = entry
                .get("root_cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let regression = entry
                .get("regression_test")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((
                status.to_string(),
                format!("{direct_cause}\n{root_cause}\n{regression}"),
            ))
        })
        .collect::<Vec<_>>();

    projection_is_consistent(&json_entries)
}

pub fn failure_registry_verified_status_pending_plan_consistent_fixture_passes() -> bool {
    fn verified_root_fix_status(status: &str) -> bool {
        status.contains("root_fix_verified") || status.contains("fix_verified")
    }

    fn pending_regression_plan(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        let value = text
            .split_once("`regression_test`:")
            .map(|(_, value)| value.trim())
            .unwrap_or(text.trim())
            .to_ascii_lowercase();
        lower.contains("must go green before")
            || lower.contains("pending lower-tier deterministic reproduction")
            || lower.contains("pending in this cycle")
            || lower.contains("are pending in this cycle")
            || lower.contains("targeted rerun pending")
            || lower.contains("fresh rerun is pending")
            || lower.contains("fresh rerun is intentionally pending")
            || lower.contains("fresh rerun is in progress")
            || lower.contains("pending follow-up live rerun")
            || lower.contains("pending follow-up fresh rerun")
            || lower.contains("fresh sweep from case")
            || lower.contains("pending: add")
            || lower.contains("pending: extend")
            || lower.contains("pending: replace")
            || lower.contains("cargo check was not rerun")
            || lower.contains("not rerun after approval rejection")
            || lower.contains("approval rejection")
            || value.starts_with("pending.")
            || value.starts_with("pending ")
    }

    fn pending_root_fix_plan(text: &str) -> bool {
        text.trim_start()
            .to_ascii_lowercase()
            .starts_with("planned:")
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        let regression_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `regression_test`:"))
            .unwrap_or_default();
        let root_fix_line = block
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("- `codex_aligned_deeper_root_fix`:")
            })
            .unwrap_or_default();
        if verified_root_fix_status(status_line)
            && (pending_regression_plan(regression_line)
                || root_fix_line
                    .split_once("`codex_aligned_deeper_root_fix`:")
                    .map(|(_, value)| pending_root_fix_plan(value))
                    .unwrap_or_else(|| pending_root_fix_plan(root_fix_line)))
        {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let regression = entry
            .get("regression_test")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let root_fix = entry
            .get("codex_aligned_deeper_root_fix")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if pending_regression_plan(regression) || pending_root_fix_plan(root_fix) {
            return false;
        }
    }

    true
}

pub fn failure_registry_verified_status_future_action_plan_consistent_fixture_passes() -> bool {
    fn verified_root_fix_status(status: &str) -> bool {
        status.contains("root_fix_verified") || status.contains("fix_verified")
    }

    fn regression_value(line: &str) -> &str {
        line.split_once("`regression_test`:")
            .map(|(_, value)| value.trim())
            .unwrap_or(line.trim())
    }

    fn future_action_plan(text: &str) -> bool {
        let value = regression_value(text).to_ascii_lowercase();
        value.starts_with("add ")
            || value.starts_with("add/strengthen")
            || value.starts_with("define ")
            || value.starts_with("replace ")
            || value.starts_with("implement ")
            || value.starts_with("extend ")
            || value.starts_with("strengthen ")
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        let regression_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `regression_test`:"))
            .unwrap_or_default();
        if verified_root_fix_status(status_line) && future_action_plan(regression_line) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let regression = entry
            .get("regression_test")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if future_action_plan(regression) {
            return false;
        }
    }

    true
}

pub fn failure_registry_verified_status_harness_assessment_current_lifecycle_fixture_passes() -> bool
{
    fn verified_root_fix_status(status: &str) -> bool {
        status.contains("root_fix_verified") || status.contains("fix_verified")
    }

    fn harness_value(line: &str) -> &str {
        line.split_once("`harness_boundary_assessment`:")
            .map(|(_, value)| value.trim())
            .unwrap_or(line.trim())
    }

    fn stale_harness_assessment(text: &str) -> bool {
        let value = harness_value(text).to_ascii_lowercase();
        value.contains("contract_gap_registered_before_fix")
            || value.starts_with("preflight_contract_gap_confirmed:")
            || value.starts_with("preflight_contract_gap_confirmed.")
            || value.contains("will be added")
            || value.contains("will be red-confirmed")
            || value.contains("does not currently reject")
            || value.contains("red-confirmed before implementation")
            || value.contains("before implementation. no live")
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        let harness_line = block
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("- `harness_boundary_assessment`:")
            })
            .unwrap_or_default();
        if verified_root_fix_status(status_line) && stale_harness_assessment(harness_line) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let harness = entry
            .get("harness_boundary_assessment")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if stale_harness_assessment(harness) {
            return false;
        }
    }

    true
}

pub fn failure_registry_regression_fixture_authority_workflow_neutral_fixture_passes() -> bool {
    fn verified_root_fix_status(status: &str) -> bool {
        status.contains("root_fix_verified") || status.contains("fix_verified")
    }

    fn regression_value(line: &str) -> &str {
        line.split_once("`regression_test`:")
            .map(|(_, value)| value.trim())
            .unwrap_or(line.trim())
    }

    fn workflow_specific_fixture_authority(text: &str) -> bool {
        let value = regression_value(text);
        let consumed_supporting_context_component_triplet = (value
            .contains("`component.py` and `test_component.py`")
            || value.contains("component.py and test_component.py"))
            && value.contains("write(docs/component-design.md)")
            && (value.contains(" read")
                || value.contains(" are read")
                || value.contains("supporting read")
                || value.contains("supporting context"));
        let rejected_tool_semantic_component_repair = value.contains("write:component.py")
            && value.contains("read(component.py)")
            && (value.contains("forbidden_supporting_tool")
                || value.contains("rejected tool")
                || value.contains("provider-submitted forbidden"));
        let docs_component_target_authority = value.contains("docs/component-design.md")
            && (value.contains("docs/text-artifact")
                || value.contains("text-artifact")
                || value.contains("serialized Markdown")
                || value.contains("Docs exact-write")
                || value.contains("docs exact-write")
                || value.contains("docs target")
                || value.contains("docs write"));
        let scenario_contract_component_pair_authority = value.contains("component.py")
            && value.contains("test_component.py")
            && value.contains("Scenario contract authority");
        let control_envelope_domain_fixture_authority = (value.contains("src/widget.ts")
            || value.contains("old.py")
            || value.contains("docs/component-design.md"))
            && (value.contains("TurnControlEnvelope")
                || value.contains("ActionAuthority")
                || value.contains("RequiredAction")
                || value.contains("ProjectionBundle")
                || value.contains("ActiveWorkContract")
                || value.contains("open obligation")
                || value.contains("open verification obligation")
                || value.contains("named tool_choice")
                || value.contains("active_targets=")
                || value.contains("ObligationAuthorityMismatch"));
        consumed_supporting_context_component_triplet
            || rejected_tool_semantic_component_repair
            || docs_component_target_authority
            || scenario_contract_component_pair_authority
            || control_envelope_domain_fixture_authority
            || [
                "generic `widget.py`",
                "generic `test_widget.py`",
                "generic widget.py",
                "generic test_widget.py",
                "with `widget.py` / `test_widget.py`",
                "with widget.py / test_widget.py",
                "fixture uses `test_widget.py`",
                "fixture uses test_widget.py",
                "`test_widget.py` remains open",
                "test_widget.py remains open",
                "`test_widget.py` on the Codex-style Code surface",
                "test_widget.py on the Codex-style Code surface",
                "`test_widget.py` derives `widget.py`",
                "test_widget.py derives widget.py",
                "FileChange to `widget.py`",
                "FileChange to widget.py",
                "widget.py / test_widget.py test-to-source mapping",
                "generic `component.py`",
                "generic `test_component.py`",
                "covers generic `component.py`",
                "covers a generic `component.py`",
                "proves a generic `component.py`",
                "target-rotated singleton `test_component.py` case",
                "names active target `test_component.py`",
                "required_action: apply_patch:test_component.py",
                "stale `component.py` only as submitted-target evidence",
                "does not project `apply_patch:component.py` as required action",
                "Active malformed apply_patch for test_component.py",
                "write:test_component.py action authority",
                "stale inactive component.py malformed patch",
                "while test_component.py is active",
                "`_run_cli`",
                "returncode assertion",
                "`proc.stdout + proc.stderr`",
                "proc.stdout + proc.stderr",
                "scenario-neutral `component.py`",
                "scenario-neutral component.py",
                "scenario-neutral fixture names `component.py`",
                "scenario-neutral fixture names component.py",
                "`component.py` source artifact",
                "component.py source artifact",
                "requires scenario-neutral fixture names `component.py`",
                "requires scenario-neutral fixture names component.py",
                "scenario-neutral `tests/test_widget.py`",
                "scenario-neutral tests/test_widget.py",
                "tests/test_widget.py / widget.advance()",
                "widget.advance()",
                "Widget.configure",
                "workflow.status",
                "test_render_output",
                "COMPLETE evidence",
                "format_public_record",
                "record_has_required_fields",
                "_prepare_record",
                "component.format_public_record",
            ]
            .into_iter()
            .any(|stale| value.contains(stale))
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        let regression_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `regression_test`:"))
            .unwrap_or_default();
        let fix_line = block
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("- `codex_aligned_deeper_root_fix`:")
            })
            .unwrap_or_default();
        let projection = format!("{fix_line}\n{regression_line}");
        if verified_root_fix_status(status_line) && workflow_specific_fixture_authority(&projection)
        {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let fix = entry
            .get("codex_aligned_deeper_root_fix")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let regression = entry
            .get("regression_test")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let projection = format!("{fix}\n{regression}");
        if workflow_specific_fixture_authority(&projection) {
            return false;
        }
    }

    true
}

pub fn failure_registry_rerun_exposed_status_verified_lifecycle_fixture_passes() -> bool {
    fn verified_root_fix_status(status: &str) -> bool {
        status.contains("root_fix_verified")
            || status.contains("fix_verified")
            || status.contains("root_fix_exposed")
    }

    fn root_fix_lifecycle_status(status: &str) -> bool {
        status.contains("root_fix") || status.contains("fix_verified")
    }

    fn successor_exposed_status(status: &str) -> bool {
        let lower = status.to_ascii_lowercase();
        if lower.contains("rerun_skipped_per_user_instruction") {
            return false;
        }
        lower.contains("exposed")
            || lower.contains("next_failure")
            || lower.contains("rerun_passed")
    }

    fn pending_full_verification_status(status: &str) -> bool {
        status.contains("pending_full_verification")
    }

    fn rerun_pending_text(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        lower.contains("rerun is next")
            || lower.contains("rerun is pending")
            || lower.contains("pending fresh rerun")
            || lower.contains("verified pending fresh rerun")
            || lower.contains("fresh rerun remains pending")
            || lower.contains("fresh rerun remain pending")
            || lower.contains("fresh rerun is intentionally pending")
            || lower.contains("fresh rerun is in progress")
            || lower.contains("remain in progress")
            || lower.contains("remains in progress")
            || lower.contains("gui fresh rerun is pending")
            || (lower.contains("pending release gui") && lower.contains("rerun"))
            || (lower.contains("pending gui required core route") && lower.contains("rerun"))
            || lower.contains("cli required core route a rerun is next")
    }

    fn unfinished_verification_text(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        lower.contains("full verification pending")
            || lower.contains("full verification remains pending")
            || lower.contains("implemented pending full verification")
            || (lower.contains("implemented at lower tier") && lower.contains("pending"))
            || (lower.contains("implemented at lower-tier") && lower.contains("pending"))
    }

    fn fresh_rerun_successor_ids(current_id: &str, status: &str) -> Vec<String> {
        let lower = status.to_ascii_lowercase();
        let marker = "fresh_rerun_exposed_";
        let Some(marker_start) = lower.find(marker) else {
            return Vec::new();
        };
        let successor = &status[marker_start + marker.len()..];
        let successor = successor
            .trim_matches(|ch: char| ch == '`' || ch == '"' || ch == '\'' || ch.is_whitespace());
        if !successor.to_ascii_lowercase().starts_with("fr") {
            return Vec::new();
        }
        let normalized = successor.replace('_', "-").to_ascii_uppercase();
        let mut candidates = vec![normalized.clone()];
        let parts = normalized.split('-').collect::<Vec<_>>();
        if parts.len() == 2 {
            if let Some((current_prefix, _)) = current_id.rsplit_once('-') {
                if current_prefix.starts_with(parts[0]) {
                    candidates.push(format!("{}-{}", current_prefix, parts[1]));
                }
            }
        }
        candidates
    }

    fn regression_projects_fresh_rerun_successor(
        current_id: &str,
        status: &str,
        regression: &str,
    ) -> bool {
        let successors = fresh_rerun_successor_ids(current_id, status);
        if successors.is_empty() {
            return true;
        }
        let lower = regression.to_ascii_lowercase();
        successors
            .iter()
            .any(|successor| regression.contains(successor))
            || lower.contains("successor failure named in status")
            || lower.contains("successor named in status")
    }

    fn later_post_fix_evidence(id: &str, later_text: &str) -> bool {
        let marker = format!("{id} post-fix");
        later_text.contains(&marker)
            && (later_text.contains("active preflight") || later_text.contains("model gate"))
            && (later_text.contains("exposed") || later_text.contains("passed"))
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_blocks = markdown.split("\n## ").skip(1).collect::<Vec<_>>();
    for (index, block) in markdown_blocks.iter().enumerate() {
        let id = block.lines().next().unwrap_or_default().trim();
        let status_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `status`:"))
            .unwrap_or_default();
        let regression_line = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `regression_test`:"))
            .unwrap_or_default();
        let status_value = status_line
            .trim_start()
            .strip_prefix("- `status`: `")
            .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
            .unwrap_or(status_line);
        if successor_exposed_status(status_line)
            && root_fix_lifecycle_status(status_line)
            && (!verified_root_fix_status(status_line)
                || rerun_pending_text(regression_line)
                || unfinished_verification_text(regression_line)
                || !regression_projects_fresh_rerun_successor(id, status_value, regression_line))
        {
            return false;
        }
        if pending_full_verification_status(status_line) {
            let later_text = markdown_blocks
                .iter()
                .skip(index + 1)
                .copied()
                .collect::<Vec<_>>()
                .join("\n## ");
            if later_post_fix_evidence(id, &later_text) {
                return false;
            }
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for (index, entry) in entries.iter().enumerate() {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or_default();
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let regression = entry
            .get("regression_test")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if successor_exposed_status(status)
            && root_fix_lifecycle_status(status)
            && (!verified_root_fix_status(status)
                || rerun_pending_text(regression)
                || unfinished_verification_text(regression)
                || !regression_projects_fresh_rerun_successor(id, status, regression))
        {
            return false;
        }
        if pending_full_verification_status(status) {
            let later_text = entries
                .iter()
                .skip(index + 1)
                .filter_map(|entry| entry.get("direct_cause").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if later_post_fix_evidence(id, &later_text) {
                return false;
            }
        }
    }

    true
}

pub fn failure_registry_verified_status_exposed_id_matches_next_failure_fixture_passes() -> bool {
    fn exposed_sequence(status: &str) -> Option<&str> {
        let tail = status.split("fresh_sweep_exposed_fr10_").nth(1)?;
        if tail.len() >= 3
            && tail.as_bytes()[..3].iter().all(u8::is_ascii_digit)
            && tail.as_bytes().get(3).is_none_or(|ch| !ch.is_ascii_digit())
        {
            return Some(&tail[..3]);
        }
        tail.rsplit('_')
            .next()
            .filter(|value| value.len() == 3 && value.chars().all(|ch| ch.is_ascii_digit()))
    }

    fn fr10_prefix_and_sequence(id: &str) -> Option<(&str, &str)> {
        let (prefix, sequence) = id.rsplit_once('-')?;
        if !prefix.starts_with("FR10-")
            || sequence.len() != 3
            || !sequence.chars().all(|ch| ch.is_ascii_digit())
        {
            return None;
        }
        Some((prefix, sequence))
    }

    fn sequence_projection_is_consistent(entries: &[(String, String)]) -> bool {
        for (index, (id, status)) in entries.iter().enumerate() {
            let Some(exposed_sequence) = exposed_sequence(status) else {
                continue;
            };
            let Some((prefix, _)) = fr10_prefix_and_sequence(id) else {
                continue;
            };
            let Some((_, next_sequence)) = entries
                .iter()
                .skip(index + 1)
                .filter_map(|(next_id, _)| fr10_prefix_and_sequence(next_id))
                .find(|(next_prefix, _)| *next_prefix == prefix)
            else {
                return false;
            };
            if exposed_sequence != next_sequence {
                return false;
            }
        }
        true
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string()))
        })
        .collect::<Vec<_>>();
    if !sequence_projection_is_consistent(&markdown_entries) {
        return false;
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((id.to_string(), status.to_string()))
        })
        .collect::<Vec<_>>();

    sequence_projection_is_consistent(&json_entries)
}

pub fn failure_registry_verified_rerun_pending_status_matches_successor_evidence_fixture_passes()
-> bool {
    fn verified_rerun_pending_status(status: &str) -> bool {
        status.contains("root_fix_verified") && status.contains("rerun_pending")
    }

    fn later_successor_evidence(id: &str, later_text: &str) -> bool {
        fn short_id(id: &str) -> Option<String> {
            let mut parts = id.split('-');
            let family = parts.next()?;
            let year = parts.next()?;
            let month = parts.next()?;
            let day = parts.next()?;
            let sequence = parts.next()?;
            if parts.next().is_some()
                || !family.starts_with("FR")
                || year.len() != 4
                || month.len() != 2
                || day.len() != 2
                || sequence.len() != 3
                || !year.chars().all(|ch| ch.is_ascii_digit())
                || !month.chars().all(|ch| ch.is_ascii_digit())
                || !day.chars().all(|ch| ch.is_ascii_digit())
                || !sequence.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            Some(format!("{family}-{sequence}"))
        }

        let full_marker = format!("{id} post-fix").to_ascii_lowercase();
        let short_marker = short_id(id)
            .map(|value| format!("{value} post-fix").to_ascii_lowercase())
            .unwrap_or_default();
        let lower = later_text.to_ascii_lowercase();
        (lower.contains(&full_marker)
            || (!short_marker.is_empty() && lower.contains(&short_marker)))
            && (lower.contains("fresh sweep") || lower.contains("fresh rerun"))
    }

    fn projection_is_consistent(entries: &[(String, String, String)]) -> bool {
        for (index, (id, status, _text)) in entries.iter().enumerate() {
            if !verified_rerun_pending_status(status) {
                continue;
            }
            let later_text = entries
                .iter()
                .skip(index + 1)
                .map(|(_, _, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if later_successor_evidence(id, &later_text) {
                return false;
            }
        }
        true
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    if !projection_is_consistent(&markdown_entries) {
        return false;
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let direct_cause = entry
                .get("direct_cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let regression = entry
                .get("regression_test")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((
                id.to_string(),
                status.to_string(),
                format!("{direct_cause}\n{regression}"),
            ))
        })
        .collect::<Vec<_>>();

    projection_is_consistent(&json_entries)
}

pub fn failure_registry_pending_fresh_rerun_status_requires_no_successor_evidence_fixture_passes()
-> bool {
    fn pending_fresh_rerun_status(status: &str) -> bool {
        matches!(
            status,
            "fix_verified_pending_fresh_rerun"
                | "fix_verified_fresh_rerun_pending"
                | "fix_verified_paused_before_fresh_rerun"
                | "post_fix_verified_pending_fresh_rerun"
                | "root_fix_verified_pending_fresh_rerun"
                | "root_fix_verified_active_preflight_model_gate_green_pending_fresh_rerun"
                | "root_fix_verified_targeted_rerun_passed_pending_fresh_sweep"
                | "root_fix_verified_entry_gates_audit_complete_pending_fresh_sweep"
                | "root_fix_verified_entry_gates_green_pending_fresh_sweep"
                | "root_fix_implemented_pending_gui_rerun"
                | "root_fix_implemented_pending_manual_st_rerun"
                | "root_fix_implemented_preflight_green"
                | "root_fix_implemented_preflight_green_pending_gui_rerun"
                | "registered_artifact_first_investigation_complete_provider_retry_pending"
        )
    }

    fn successor_rerun_evidence(id: &str, later_text: &str) -> bool {
        fn short_id(id: &str) -> Option<String> {
            let mut parts = id.split('-');
            let family = parts.next()?;
            let year = parts.next()?;
            let month = parts.next()?;
            let day = parts.next()?;
            let sequence = parts.next()?;
            if parts.next().is_some()
                || !family.starts_with("FR")
                || year.len() != 4
                || month.len() != 2
                || day.len() != 2
                || sequence.len() != 3
                || !year.chars().all(|ch| ch.is_ascii_digit())
                || !month.chars().all(|ch| ch.is_ascii_digit())
                || !day.chars().all(|ch| ch.is_ascii_digit())
                || !sequence.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            Some(format!("{family}-{sequence}"))
        }

        let full_marker = format!("{id} post-fix").to_ascii_lowercase();
        let short_marker = short_id(id)
            .map(|value| format!("{value} post-fix").to_ascii_lowercase())
            .unwrap_or_default();
        let lower = later_text.to_ascii_lowercase();
        (lower.contains(&full_marker)
            || (!short_marker.is_empty() && lower.contains(&short_marker)))
            && (lower.contains("rerun") || lower.contains("fresh sweep"))
    }

    fn same_entry_exposed_successor_evidence(entry_text: &str) -> bool {
        let lower = entry_text.to_ascii_lowercase();
        (lower.contains("exposed fr")
            || lower.contains("exposed `fr")
            || lower.contains("post-fix rerun later exposed the successor failure")
            || lower.contains("post-fix rerun exposed the successor failure")
            || lower.contains("successor failure named in status"))
            && (lower.contains("rerun") || lower.contains("fresh sweep"))
    }

    fn same_entry_executed_fresh_sweep_evidence(entry_text: &str) -> bool {
        let lower = entry_text.to_ascii_lowercase();
        lower.contains("post-fix")
            && lower.contains("fresh sweep")
            && (lower.contains("stopped")
                || lower.contains("failed")
                || lower.contains("exposed")
                || lower.contains("passed case1"))
    }

    fn adjacent_rerun_number_successor_evidence(id: &str, next_id: &str, next_text: &str) -> bool {
        fn parse_id(id: &str) -> Option<(&str, &str, &str, &str, u16)> {
            let mut parts = id.split('-');
            let family = parts.next()?;
            let year = parts.next()?;
            let month = parts.next()?;
            let day = parts.next()?;
            let sequence = parts.next()?;
            if parts.next().is_some()
                || !family.starts_with("FR")
                || year.len() != 4
                || month.len() != 2
                || day.len() != 2
                || sequence.len() != 3
                || !year.chars().all(|ch| ch.is_ascii_digit())
                || !month.chars().all(|ch| ch.is_ascii_digit())
                || !day.chars().all(|ch| ch.is_ascii_digit())
                || !sequence.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            let sequence = sequence.parse::<u16>().ok()?;
            Some((family, year, month, day, sequence))
        }

        let Some((family, year, month, day, sequence)) = parse_id(id) else {
            return false;
        };
        let Some((next_family, next_year, next_month, next_day, next_sequence)) = parse_id(next_id)
        else {
            return false;
        };
        if family != next_family
            || year != next_year
            || month != next_month
            || day != next_day
            || next_sequence != sequence + 1
        {
            return false;
        }

        let next_sequence_text = format!("{next_sequence:03}");
        let lower = next_text.to_ascii_lowercase();
        lower.contains(&format!("rerun-{next_sequence_text}"))
            || lower.contains(&format!("rerun {next_sequence_text}"))
    }

    fn adjacent_registered_successor_lifecycle_evidence(
        id: &str,
        next_id: &str,
        next_status: &str,
        next_text: &str,
    ) -> bool {
        fn parse_id(id: &str) -> Option<(&str, &str, &str, &str, u16)> {
            let mut parts = id.split('-');
            let family = parts.next()?;
            let year = parts.next()?;
            let month = parts.next()?;
            let day = parts.next()?;
            let sequence = parts.next()?;
            if parts.next().is_some()
                || !family.starts_with("FR")
                || year.len() != 4
                || month.len() != 2
                || day.len() != 2
                || sequence.len() != 3
                || !year.chars().all(|ch| ch.is_ascii_digit())
                || !month.chars().all(|ch| ch.is_ascii_digit())
                || !day.chars().all(|ch| ch.is_ascii_digit())
                || !sequence.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            let sequence = sequence.parse::<u16>().ok()?;
            Some((family, year, month, day, sequence))
        }

        let Some((family, year, month, day, sequence)) = parse_id(id) else {
            return false;
        };
        let Some((next_family, next_year, next_month, next_day, next_sequence)) = parse_id(next_id)
        else {
            return false;
        };
        if family != next_family
            || year != next_year
            || month != next_month
            || day != next_day
            || next_sequence != sequence + 1
        {
            return false;
        }

        let status = next_status.to_ascii_lowercase();
        let _text = next_text.to_ascii_lowercase();
        (status.contains("root_fix_verified")
            || status.contains("fix_verified")
            || status.contains("root_fix_exposed"))
            && (status.contains("next_failure") || status.contains("exposed"))
    }

    fn adjacent_fresh_rerun_successor_evidence(next_text: &str) -> bool {
        let lower = next_text.to_ascii_lowercase();
        lower.contains("fresh rerun")
            && (lower.contains("model availability") || lower.contains("model gate"))
            && lower.contains("active preflight")
            && (lower.contains("exposed") || lower.contains("next failure"))
    }

    fn adjacent_live_rerun_followup_successor_evidence(next_status: &str, next_text: &str) -> bool {
        let status = next_status.to_ascii_lowercase();
        let lower = next_text.to_ascii_lowercase();
        (status.contains("followup_failure_exposed")
            || status.contains("next_failure_exposed")
            || status.contains("fresh_rerun_exposed"))
            && (lower.contains("live rerun") || lower.contains("fresh rerun"))
            && (lower.contains("model gate") || lower.contains("model availability"))
            && lower.contains("active preflight")
    }

    fn adjacent_post_fix_rerun_successor_evidence(
        id: &str,
        next_status: &str,
        next_text: &str,
    ) -> bool {
        fn short_id(id: &str) -> Option<String> {
            let mut parts = id.split('-');
            let family = parts.next()?;
            let year = parts.next()?;
            let month = parts.next()?;
            let day = parts.next()?;
            let sequence = parts.next()?;
            if parts.next().is_some()
                || !family.starts_with("FR")
                || year.len() != 4
                || month.len() != 2
                || day.len() != 2
                || sequence.len() != 3
                || !year.chars().all(|ch| ch.is_ascii_digit())
                || !month.chars().all(|ch| ch.is_ascii_digit())
                || !day.chars().all(|ch| ch.is_ascii_digit())
                || !sequence.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            Some(format!("{family}-{sequence}"))
        }

        let status = next_status.to_ascii_lowercase();
        if status.contains("false_stop") || status.contains("no_code_fix") {
            return false;
        }
        let lower = next_text.to_ascii_lowercase();
        let id_lower = id.to_ascii_lowercase();
        let short = short_id(id)
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        let current_post_fix = (lower.contains(&id_lower) && lower.contains("post-fix"))
            || (!short.is_empty()
                && (lower.contains(&format!("{short}-post-fix"))
                    || lower.contains(&format!("{short} post-fix"))));
        current_post_fix && lower.contains("rerun")
    }

    fn projection_is_consistent(entries: &[(String, String, String)]) -> bool {
        for (index, (id, status, text)) in entries.iter().enumerate() {
            if !pending_fresh_rerun_status(status) {
                continue;
            }
            if same_entry_exposed_successor_evidence(text) {
                return false;
            }
            if status.contains("pending_fresh_sweep")
                && same_entry_executed_fresh_sweep_evidence(text)
            {
                return false;
            }
            if let Some((next_id, _next_status, next_text)) = entries.get(index + 1) {
                if adjacent_rerun_number_successor_evidence(id, next_id, next_text) {
                    return false;
                }
                if adjacent_registered_successor_lifecycle_evidence(
                    id,
                    next_id,
                    _next_status,
                    next_text,
                ) {
                    return false;
                }
                if adjacent_fresh_rerun_successor_evidence(next_text) {
                    return false;
                }
                if adjacent_live_rerun_followup_successor_evidence(_next_status, next_text) {
                    return false;
                }
                if adjacent_post_fix_rerun_successor_evidence(id, _next_status, next_text) {
                    return false;
                }
            }
            let later_text = entries
                .iter()
                .skip(index + 1)
                .map(|(_, _, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if successor_rerun_evidence(id, &later_text) {
                return false;
            }
        }
        true
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    if !projection_is_consistent(&markdown_entries) {
        return false;
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let direct_cause = entry
                .get("direct_cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let regression = entry
                .get("regression_test")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((
                id.to_string(),
                status.to_string(),
                format!("{direct_cause}\n{regression}"),
            ))
        })
        .collect::<Vec<_>>();

    projection_is_consistent(&json_entries)
}

pub fn failure_registry_root_fix_pending_gui_rerun_status_cannot_outlive_successor_evidence_fixture_passes()
-> bool {
    failure_registry_pending_fresh_rerun_status_requires_no_successor_evidence_fixture_passes()
}

pub fn failure_registry_post_fix_verified_status_requires_successor_projection_fixture_passes()
-> bool {
    fn parse_id(id: &str) -> Option<(&str, &str, &str, &str, u16)> {
        let mut parts = id.split('-');
        let family = parts.next()?;
        let year = parts.next()?;
        let month = parts.next()?;
        let day = parts.next()?;
        let sequence = parts.next()?;
        if parts.next().is_some()
            || !family.starts_with("FR")
            || year.len() != 4
            || month.len() != 2
            || day.len() != 2
            || sequence.len() != 3
            || !year.chars().all(|ch| ch.is_ascii_digit())
            || !month.chars().all(|ch| ch.is_ascii_digit())
            || !day.chars().all(|ch| ch.is_ascii_digit())
            || !sequence.chars().all(|ch| ch.is_ascii_digit())
        {
            return None;
        }
        let sequence = sequence.parse::<u16>().ok()?;
        Some((family, year, month, day, sequence))
    }

    fn adjacent_successor_post_fix_rerun_evidence(
        current_id: &str,
        next_id: &str,
        next_text: &str,
    ) -> bool {
        let Some((family, year, month, day, sequence)) = parse_id(current_id) else {
            return false;
        };
        let Some((next_family, next_year, next_month, next_day, next_sequence)) = parse_id(next_id)
        else {
            return false;
        };
        if family != next_family
            || year != next_year
            || month != next_month
            || day != next_day
            || next_sequence != sequence + 1
        {
            return false;
        }

        let lower = next_text.to_ascii_lowercase();
        let current_short = format!("{family}-{sequence:03}").to_ascii_lowercase();
        let full_post_fix_marker = current_id.to_ascii_lowercase();
        let has_current_post_fix_gate = lower.contains(&format!("{current_short}-post-fix"))
            || lower.contains(&format!("{current_short} post-fix"))
            || (lower.contains(&full_post_fix_marker) && lower.contains("post-fix"));
        let next_rerun_marker = format!("rerun-{next_sequence:03}");
        let has_next_rerun = lower.contains(&next_rerun_marker)
            || lower.contains(&format!("rerun {next_sequence:03}"));

        has_current_post_fix_gate && has_next_rerun
    }

    fn adjacent_successor_entry_rerun_evidence(
        current_id: &str,
        next_id: &str,
        next_text: &str,
    ) -> bool {
        let Some((family, year, month, day, sequence)) = parse_id(current_id) else {
            return false;
        };
        let Some((next_family, next_year, next_month, next_day, next_sequence)) = parse_id(next_id)
        else {
            return false;
        };
        if family != next_family
            || year != next_year
            || month != next_month
            || day != next_day
            || next_sequence != sequence + 1
        {
            return false;
        }

        let lower = next_text.to_ascii_lowercase();
        let next_rerun_marker = format!("rerun-{next_sequence:03}");
        (lower.contains(&next_rerun_marker) || lower.contains(&format!("rerun {next_sequence:03}")))
            && (lower.contains("model gate") || lower.contains("model availability"))
            && lower.contains("active preflight")
            && (lower.contains("failure") || lower.contains("failed") || lower.contains("exposed"))
    }

    fn projection_is_consistent(entries: &[(String, String, String)]) -> bool {
        for (index, (id, status, _text)) in entries.iter().enumerate() {
            if status != "post_fix_verified" && status != "root_fix_verified" {
                continue;
            }
            if let Some((next_id, _next_status, next_text)) = entries.get(index + 1) {
                if adjacent_successor_post_fix_rerun_evidence(id, next_id, next_text) {
                    return false;
                }
                if status == "root_fix_verified"
                    && adjacent_successor_entry_rerun_evidence(id, next_id, next_text)
                {
                    return false;
                }
            }
        }
        true
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    if !projection_is_consistent(&markdown_entries) {
        return false;
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let direct_cause = entry
                .get("direct_cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reproduction = entry
                .get("reproduction_command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let regression = entry
                .get("regression_test")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let artifact_refs = entry
                .get("artifact_refs")
                .and_then(Value::as_array)
                .map(|refs| {
                    refs.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Some((
                id.to_string(),
                status.to_string(),
                format!("{direct_cause}\n{reproduction}\n{regression}\n{artifact_refs}"),
            ))
        })
        .collect::<Vec<_>>();

    projection_is_consistent(&json_entries)
}

pub fn failure_registry_next_failure_exposed_status_names_successor_id_fixture_passes() -> bool {
    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status = block
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
            .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
            .unwrap_or_default();
        if status == "root_fix_verified_next_failure_exposed"
            || status == "root_fix_verified_followup_failure_exposed"
            || status == "fix_verified_followup_failure_exposed"
        {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == "root_fix_verified_next_failure_exposed"
            || status == "root_fix_verified_followup_failure_exposed"
            || status == "fix_verified_followup_failure_exposed"
        {
            return false;
        }
    }

    true
}

pub fn failure_registry_verified_rerun_status_cannot_remain_transient_fixture_passes() -> bool {
    fn transient_verified_rerun_status(status: &str) -> bool {
        status == "root_fix_verified_rerun_pending"
            || status == "root_fix_verified_rerun_in_progress"
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        let status = block
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
            .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
            .unwrap_or_default();
        if transient_verified_rerun_status(status) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if transient_verified_rerun_status(status) {
            return false;
        }
    }

    true
}

pub fn failure_registry_verified_pending_status_blocker_resolution_fixture_passes() -> bool {
    fn verified_pending_status(status: &str) -> bool {
        status.contains("root_fix_verified_pending_route")
            || status.contains("root_fix_verified_pending_fr10")
            || status.contains("root_fix_verified_pending_cli_fresh_rerun")
            || status == "root_fix_verified_pending_fresh_rerun"
    }

    fn resolved_lifecycle_status(status: &str) -> bool {
        status.contains("root_fix_verified")
            && (status.contains("rerun_exposed") || status.contains("rerun_passed"))
    }

    fn stale_next_rerun_text(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        lower.contains("cli fresh rerun is next")
            || lower.contains("fresh rerun is next")
            || lower.contains("rerun remains blocked behind")
    }

    fn entry_violates<'a>(
        id: &str,
        status: &str,
        text: &str,
        mut all_entries: impl Iterator<Item = (&'a str, &'a str)>,
        later_text: &str,
    ) -> bool {
        if !verified_pending_status(status) {
            return false;
        }
        if status == "root_fix_verified_pending_fresh_rerun" {
            let post_fix_marker = format!("{id} post-fix").to_ascii_lowercase();
            let short_post_fix_marker = id
                .rsplit_once('-')
                .and_then(|(prefix, sequence)| {
                    prefix
                        .split_once('-')
                        .map(|(family, _)| format!("{family}-{sequence} post-fix"))
                })
                .unwrap_or_default()
                .to_ascii_lowercase();
            let later_lower = later_text.to_ascii_lowercase();
            if later_lower.contains(&post_fix_marker)
                || (!short_post_fix_marker.is_empty()
                    && later_lower.contains(&short_post_fix_marker))
            {
                return true;
            }
            return false;
        }
        if stale_next_rerun_text(text) {
            return true;
        }
        let normalized_text = text.replace('_', "-").to_ascii_lowercase();
        all_entries.any(|(id, other_status)| {
            normalized_text.contains(&id.to_ascii_lowercase())
                && resolved_lifecycle_status(other_status)
        })
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    for (index, (id, status, text)) in markdown_entries.iter().enumerate() {
        let later_text = markdown_entries
            .iter()
            .skip(index + 1)
            .map(|(_, _, block)| block.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if entry_violates(
            id,
            status,
            text,
            markdown_entries
                .iter()
                .map(|(id, other_status, _)| (id.as_str(), other_status.as_str())),
            &later_text,
        ) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = serde_json::to_string(entry).unwrap_or_default();
            Some((id.to_string(), status.to_string(), text))
        })
        .collect::<Vec<_>>();
    for (index, (id, status, text)) in json_entries.iter().enumerate() {
        let later_text = json_entries
            .iter()
            .skip(index + 1)
            .map(|(_, _, entry_text)| entry_text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if entry_violates(
            id,
            status,
            text,
            json_entries
                .iter()
                .map(|(id, other_status, _)| (id.as_str(), other_status.as_str())),
            &later_text,
        ) {
            return false;
        }
    }

    true
}

pub fn failure_registry_root_identified_status_successor_evidence_fixture_passes() -> bool {
    fn nonterminal_root_identified_status(status: &str, text: &str) -> bool {
        let status = status.to_ascii_lowercase();
        let text = text.to_ascii_lowercase();
        status.contains("root_identified")
            || status.contains("pending_lower_tier")
            || text.contains("pending lower-tier deterministic reproduction")
    }

    fn short_failure_id(id: &str) -> Option<String> {
        let parts = id.split('-').collect::<Vec<_>>();
        if parts.len() < 5 {
            return None;
        }
        Some(format!("{}-{}", parts[0], parts[4]))
    }

    fn successor_mentions_completed_current_failure(id: &str, next_text: &str) -> bool {
        let next = next_text.replace('_', "-").to_ascii_lowercase();
        let full_marker = format!("after {}", id.to_ascii_lowercase());
        let short_marker = short_failure_id(id)
            .map(|short| format!("after {}", short.to_ascii_lowercase()))
            .unwrap_or_default();
        next.contains(&full_marker) || (!short_marker.is_empty() && next.contains(&short_marker))
    }

    fn entry_violates(id: &str, status: &str, text: &str, next_text: Option<&str>) -> bool {
        if !nonterminal_root_identified_status(status, text) {
            return false;
        }
        next_text
            .map(|next| successor_mentions_completed_current_failure(id, next))
            .unwrap_or(false)
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    for index in 0..markdown_entries.len() {
        let (id, status, text) = &markdown_entries[index];
        let next_text = markdown_entries
            .get(index + 1)
            .map(|(_, _, block)| block.as_str());
        if entry_violates(id, status, text, next_text) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = serde_json::to_string(entry).unwrap_or_default();
            Some((id.to_string(), status.to_string(), text))
        })
        .collect::<Vec<_>>();
    for index in 0..json_entries.len() {
        let (id, status, text) = &json_entries[index];
        let next_text = json_entries
            .get(index + 1)
            .map(|(_, _, entry_text)| entry_text.as_str());
        if entry_violates(id, status, text, next_text) {
            return false;
        }
    }

    true
}

pub fn failure_registry_root_fix_in_progress_status_successor_evidence_fixture_passes() -> bool {
    fn nonterminal_root_fix_status(status: &str) -> bool {
        let status = status.to_ascii_lowercase();
        status.contains("pending_root_fix")
            || status.contains("root_fix_in_progress")
            || status.contains("pending_gui_manual_structure_review")
    }

    fn short_failure_id(id: &str) -> Option<String> {
        let parts = id.split('-').collect::<Vec<_>>();
        if parts.len() < 5 {
            return None;
        }
        Some(format!("{}-{}", parts[0], parts[4]))
    }

    fn later_successor_evidence(id: &str, later_text: &str) -> bool {
        let later = later_text.replace('_', "-").to_ascii_lowercase();
        let full_id = id.to_ascii_lowercase();
        let full_post_fix = format!("{full_id} post-fix");
        let full_after = format!("after {full_id}");
        let short_id = short_failure_id(id)
            .map(|short| short.to_ascii_lowercase())
            .unwrap_or_default();
        let short_post_fix = if short_id.is_empty() {
            String::new()
        } else {
            format!("{short_id} post-fix")
        };
        let short_after = if short_id.is_empty() {
            String::new()
        } else {
            format!("after {short_id}")
        };
        let mentions_current = later.contains(&full_post_fix)
            || later.contains(&full_after)
            || (!short_post_fix.is_empty() && later.contains(&short_post_fix))
            || (!short_after.is_empty() && later.contains(&short_after));
        let successor_lifecycle = later.contains("implemented generic")
            || later.contains("implemented-and-verified")
            || later.contains("implemented and verified")
            || later.contains("root-fix verified")
            || later.contains("root fix verified")
            || later.contains("post-fix fresh rerun")
            || later.contains("post-fix rerun")
            || later.contains("exposed fr")
            || later.contains("later exposed");
        let gui_manual_review_resolved = later
            .contains("transcript export structure rule was synchronized")
            && later.contains("gui rerun")
            && (later.contains("started") || later.contains("exposed"));
        (mentions_current && successor_lifecycle) || gui_manual_review_resolved
    }

    fn entry_violates(id: &str, status: &str, later_text: &str) -> bool {
        nonterminal_root_fix_status(status) && later_successor_evidence(id, later_text)
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    let markdown_entries = markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|block| {
            let id = block.lines().next()?.trim();
            let status = block
                .lines()
                .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
                .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
                .unwrap_or_default();
            Some((id.to_string(), status.to_string(), block.to_string()))
        })
        .collect::<Vec<_>>();
    for (index, (id, status, _text)) in markdown_entries.iter().enumerate() {
        let later_text = markdown_entries
            .iter()
            .skip(index + 1)
            .map(|(_, _, block)| block.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if entry_violates(id, status, &later_text) {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let json_entries = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = serde_json::to_string(entry).unwrap_or_default();
            Some((id.to_string(), status.to_string(), text))
        })
        .collect::<Vec<_>>();
    for (index, (id, status, _text)) in json_entries.iter().enumerate() {
        let later_text = json_entries
            .iter()
            .skip(index + 1)
            .map(|(_, _, entry_text)| entry_text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if entry_violates(id, status, &later_text) {
            return false;
        }
    }

    true
}

pub fn failure_registry_verified_status_pending_investigation_projection_fixture_passes() -> bool {
    fn verified_root_fix_status(status: &str) -> bool {
        let status = status.to_ascii_lowercase();
        status.contains("root_fix_verified") || status.contains("fix_verified")
    }

    fn pending_investigation_projection(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        let process_ref_count = [
            "artifact-first investigation",
            "codex lifecycle comparison",
            "item lifecycle survey/design refresh",
            "root fix point enumeration",
        ]
        .into_iter()
        .filter(|needle| lower.contains(needle))
        .count();
        let pending_ref = lower.contains(" are pending")
            || lower.contains(" is pending")
            || lower.contains(" pending because")
            || lower.contains("pending because");
        process_ref_count >= 2 && pending_ref
    }

    fn markdown_field_after_artifact_refs(block: &str) -> bool {
        let mut in_artifact_refs = false;
        for line in block.lines() {
            if line.starts_with("- `artifact_refs`:") {
                in_artifact_refs = true;
                continue;
            }
            if in_artifact_refs && line.starts_with("- `") {
                return true;
            }
        }
        false
    }

    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let markdown_path = root.join("docs").join("testing").join("FailureRegistry.md");
    let json_path = root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let Ok(markdown) = fs::read_to_string(markdown_path) else {
        return false;
    };
    let Ok(json_content) = fs::read_to_string(json_path) else {
        return false;
    };

    for block in markdown.split("\n## ").skip(1) {
        if markdown_field_after_artifact_refs(block) {
            return false;
        }
        let status = block
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("- `status`: `"))
            .and_then(|rest| rest.split_once('`').map(|(status, _)| status))
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let root_cause = block
            .lines()
            .find(|line| line.trim_start().starts_with("- `root_cause`:"))
            .unwrap_or_default();
        let root_fix = block
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("- `codex_aligned_deeper_root_fix`:")
            })
            .unwrap_or_default();
        if pending_investigation_projection(root_cause)
            || pending_investigation_projection(root_fix)
        {
            return false;
        }
    }

    let Ok(json): Result<Value, _> = serde_json::from_str(&json_content) else {
        return false;
    };
    let Some(entries) = json.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_root_fix_status(status) {
            continue;
        }
        let root_cause = entry
            .get("root_cause")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let root_fix = entry
            .get("codex_aligned_deeper_root_fix")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if pending_investigation_projection(root_cause)
            || pending_investigation_projection(root_fix)
        {
            return false;
        }
    }

    true
}

fn collect_manual_st_reference_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_manual_st_reference_files(&path, files);
        } else {
            files.push(path);
        }
    }
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
    let schema_failures = if missing.is_empty() {
        validate_manual_st_artifact_schema(artifact_root, &required_artifacts)?
    } else {
        Vec::new()
    };

    let mut diagnostics = Vec::new();
    if !failure_ids.is_empty() {
        diagnostics.push(format!(
            "failure evidence ids are replay metadata only: {}",
            failure_ids.join(",")
        ));
    }
    if missing.is_empty() && schema_failures.is_empty() {
        diagnostics.push("artifact root satisfies Codex-style route evidence schema".to_string());
    } else {
        if !missing.is_empty() {
            diagnostics.push(format!(
                "artifact root is missing required route evidence artifacts: {}",
                missing.join(", ")
            ));
        }
        if !schema_failures.is_empty() {
            diagnostics.push(format!(
                "artifact root has malformed route evidence artifacts: {}",
                schema_failures.join("; ")
            ));
        }
    }

    Ok(PreflightReport::from_results(vec![PreflightGateReport {
        gate_id: "preflight.artifact.route_evidence_schema".to_string(),
        fixture_id: Some("fixture.artifact.route_evidence_schema".to_string()),
        layer: PreflightLayer::HarnessReplay,
        family: Some(PreflightGateFamily::ArtifactReplaySchema),
        status: if missing.is_empty() && schema_failures.is_empty() {
            PreflightResultStatus::Pass
        } else {
            PreflightResultStatus::Fail
        },
        diagnostics,
        evidence_refs: required_artifacts,
    }]))
}

fn validate_manual_st_artifact_schema(
    artifact_root: &Utf8Path,
    required_artifacts: &[String],
) -> Result<Vec<String>, RuntimeError> {
    let mut failures = Vec::new();
    let route_manifest =
        read_required_artifact_json(artifact_root, "route_manifest.json", &mut failures)?;
    let case_progress =
        read_required_artifact_json(artifact_root, "case_progress.json", &mut failures)?;
    let verification_log = read_required_artifact_json(
        artifact_root,
        "verification_command_log.json",
        &mut failures,
    )?;
    let workspace_diff =
        read_required_artifact_json(artifact_root, "workspace_diff_manifest.json", &mut failures)?;
    let result = read_required_artifact_json(artifact_root, "result.json", &mut failures)?;
    let preflight_report =
        read_required_artifact_json(artifact_root, "preflight_report.json", &mut failures)?;
    let timeout_classification =
        read_required_artifact_json(artifact_root, "timeout_classification.json", &mut failures)?;

    validate_route_manifest(&route_manifest, required_artifacts, &mut failures);
    validate_case_progress(&case_progress, &mut failures);
    validate_route_result(&result, &mut failures);
    validate_route_preflight_report(&preflight_report, &mut failures);
    validate_timeout_classification(&timeout_classification, &mut failures);
    require_array(
        &verification_log,
        "verification_command_log.json",
        "commands",
        &mut failures,
    );
    validate_workspace_diff_manifest(&workspace_diff, &mut failures);

    Ok(failures)
}

fn read_required_artifact_json(
    artifact_root: &Utf8Path,
    name: &str,
    failures: &mut Vec<String>,
) -> Result<Value, RuntimeError> {
    let path = artifact_root.join(name);
    let bytes = fs::read(&path).map_err(|error| {
        RuntimeError::Message(format!("failed to read route artifact `{path}`: {error}"))
    })?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => {
            failures.push(format!("{name} must be a JSON object"));
            Ok(Value::Null)
        }
        Err(error) => {
            failures.push(format!("{name} is not valid JSON: {error}"));
            Ok(Value::Null)
        }
    }
}

fn validate_route_manifest(
    value: &Value,
    required_artifacts: &[String],
    failures: &mut Vec<String>,
) {
    require_string(value, "route_manifest.json", "route_id", failures);
    require_string(value, "route_manifest.json", "route_type", failures);
    require_string_value(
        value,
        "route_manifest.json",
        "fixture_version",
        "manual_st_route_runner.v1",
        failures,
    );
    require_route_verdict(value, "route_manifest.json", failures);
    require_non_empty_string_array(value, "route_manifest.json", "case_ids", failures);
    let Some(evidence_artifacts) = value.get("evidence_artifacts").and_then(Value::as_array) else {
        failures.push("route_manifest.json.evidence_artifacts must be an array".to_string());
        return;
    };
    for artifact in required_artifacts {
        if !evidence_artifacts
            .iter()
            .any(|value| value.as_str() == Some(artifact.as_str()))
        {
            failures.push(format!(
                "route_manifest.json.evidence_artifacts must include {artifact}"
            ));
        }
    }
}

fn validate_case_progress(value: &Value, failures: &mut Vec<String>) {
    require_string(value, "case_progress.json", "route_id", failures);
    require_string(value, "case_progress.json", "route_type", failures);
    require_route_verdict(value, "case_progress.json", failures);
    require_string(value, "case_progress.json", "progress_status", failures);
    require_string_value(
        value,
        "case_progress.json",
        "evidence_artifact_schema_version",
        "manual_st.case_progress.v1",
        failures,
    );
}

fn validate_route_result(value: &Value, failures: &mut Vec<String>) {
    require_string(value, "result.json", "route_id", failures);
    require_string(value, "result.json", "route_type", failures);
    require_route_verdict(value, "result.json", failures);
    require_non_empty_string_array(value, "result.json", "case_ids", failures);
    require_array(value, "result.json", "case_results", failures);
}

fn validate_route_preflight_report(value: &Value, failures: &mut Vec<String>) {
    require_string_value(
        value,
        "preflight_report.json",
        "generated_by",
        "codex_style_preflight_v2",
        failures,
    );
    require_string_value(value, "preflight_report.json", "status", "pass", failures);
    let Some(results) = value.get("results").and_then(Value::as_array) else {
        failures.push("preflight_report.json.results must be an array".to_string());
        return;
    };
    if results.is_empty() {
        failures.push("preflight_report.json.results must not be empty".to_string());
    }
    for (index, result) in results.iter().enumerate() {
        if non_empty_string(result.get("fixture_id")).is_none() {
            failures.push(format!(
                "preflight_report.json.results[{index}].fixture_id must be a non-empty string"
            ));
        }
        if result.get("status").and_then(Value::as_str) != Some("pass") {
            failures.push(format!(
                "preflight_report.json.results[{index}].status must be pass"
            ));
        }
    }
}

fn validate_timeout_classification(value: &Value, failures: &mut Vec<String>) {
    for field in [
        "classified_terminal_before_timeout",
        "outer_timeout",
        "provider_stream_retry_exhausted",
        "provider_stream_stall",
        "provider_transport_stream_error",
        "semantic_no_progress_terminal_guard",
        "tool_or_environment_stall",
        "verification_non_convergence",
    ] {
        require_bool(value, "timeout_classification.json", field, failures);
    }
    require_array(
        value,
        "timeout_classification.json",
        "evidence_refs",
        failures,
    );
}

fn validate_workspace_diff_manifest(value: &Value, failures: &mut Vec<String>) {
    for field in [
        "expected_artifacts",
        "actual_added_files",
        "actual_modified_files",
        "actual_deleted_files",
        "diagnostics",
    ] {
        require_array(value, "workspace_diff_manifest.json", field, failures);
    }
    for field in [
        "unexpected_outside_workspace_access_or_change",
        "fixture_input_mutation",
    ] {
        require_bool(value, "workspace_diff_manifest.json", field, failures);
    }
    require_string(value, "workspace_diff_manifest.json", "verdict", failures);
}

fn require_route_verdict(value: &Value, artifact: &str, failures: &mut Vec<String>) {
    match value.get("route_level_verdict").and_then(Value::as_str) {
        Some("pass" | "fail" | "running" | "not_run") => {}
        _ => failures.push(format!(
            "{artifact}.route_level_verdict must be pass, fail, running, or not_run"
        )),
    }
}

fn require_string(value: &Value, artifact: &str, field: &str, failures: &mut Vec<String>) {
    if non_empty_string(value.get(field)).is_none() {
        failures.push(format!("{artifact}.{field} must be a non-empty string"));
    }
}

fn require_string_value(
    value: &Value,
    artifact: &str,
    field: &str,
    expected: &str,
    failures: &mut Vec<String>,
) {
    if value.get(field).and_then(Value::as_str) != Some(expected) {
        failures.push(format!("{artifact}.{field} must equal {expected}"));
    }
}

fn require_non_empty_string_array(
    value: &Value,
    artifact: &str,
    field: &str,
    failures: &mut Vec<String>,
) {
    let Some(items) = value.get(field).and_then(Value::as_array) else {
        failures.push(format!("{artifact}.{field} must be an array"));
        return;
    };
    if items.is_empty() {
        failures.push(format!("{artifact}.{field} must not be empty"));
    }
    for (index, item) in items.iter().enumerate() {
        if non_empty_string(Some(item)).is_none() {
            failures.push(format!(
                "{artifact}.{field}[{index}] must be a non-empty string"
            ));
        }
    }
}

fn require_array(value: &Value, artifact: &str, field: &str, failures: &mut Vec<String>) {
    if !value.get(field).is_some_and(Value::is_array) {
        failures.push(format!("{artifact}.{field} must be an array"));
    }
}

fn require_bool(value: &Value, artifact: &str, field: &str, failures: &mut Vec<String>) {
    if !value.get(field).is_some_and(Value::is_boolean) {
        failures.push(format!("{artifact}.{field} must be a boolean"));
    }
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn artifact_replay_rejects_empty_route_evidence_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    for artifact in required_manual_st_artifacts() {
        if fs::write(temp.path().join(artifact), "{}").is_err() {
            return false;
        }
    }
    let Ok(root) = camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    let Ok(report) = run_artifact_replay_preflight(&root, Vec::new()) else {
        return false;
    };
    matches!(report.status, PreflightResultStatus::Fail)
        && report.results.iter().any(|result| {
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.contains("malformed route evidence artifacts")
                    && diagnostic.contains("route_manifest.json.route_id")
                    && diagnostic.contains("preflight_report.json.generated_by")
            })
        })
}

pub fn desktop_uat_plan_current_profile_visible_evidence_policy_fixture_passes() -> bool {
    let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return false;
    };
    let plan_path = root.join("docs").join("testing").join("DesktopUATPlan.md");
    let Ok(plan) = fs::read_to_string(plan_path) else {
        return false;
    };
    plan.contains("http://127.0.0.1:1234")
        && plan.contains("qwen/qwen3.6-35b-a3b")
        && plan.contains("lm_studio_native_required")
        && plan.contains("131072")
        && plan.contains("`moyAI` を Windows Desktop GUI")
        && plan.contains("moyAI/tests/manual_ST")
        && plan.contains("canonical pseudo tool-call assistant evidence")
        && plan.contains("visible evidence")
        && plan.contains("clean closeout authority")
        && !plan.contains("http://192.168.10.103:1234")
        && !plan.contains("openai-compatible-fixture-model")
        && !plan.contains("openai_compatible_only")
        && !plan.contains("`moyai` を Windows Desktop GUI")
        && !plan.contains("moyai/tests/manual_ST")
        && !plan.contains("raw pseudo tool-call prose は primary GUI reading path に出ず")
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

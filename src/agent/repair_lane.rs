use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use sha2::{Digest, Sha256};

use crate::agent::contract_reconciliation::{
    ContractFailureOwner, ContractReconciliationDecision,
    reconcile_session_state_failure_with_cluster,
};
use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target, language_file_refs_from_summary,
    language_generated_test_contract_drift_markers_from_summary as generated_test_contract_drift_markers_from_summary,
    language_generated_test_logging_contract_overreach as generated_test_logging_contract_overreach,
    language_generated_test_module_attribute_api_misuse as generated_test_module_attribute_api_misuse,
    language_generated_test_name_resolution_defect as generated_test_name_resolution_defect,
    language_generated_test_public_output_contract_overreach as generated_test_public_output_contract_overreach,
    language_generated_test_reflection_api_misuse as generated_test_reflection_api_misuse,
    language_generated_test_subprocess_encoding_missing as generated_test_subprocess_encoding_missing,
    language_generated_test_subprocess_output_capture_missing as generated_test_subprocess_output_capture_missing,
    language_public_api_data_model_semantic_obligations as public_api_data_model_semantic_obligations,
    language_public_callable_signature_mismatch as public_callable_signature_mismatch,
    language_public_class_member_repair_observations as public_class_member_repair_observations,
    language_public_class_or_enum_missing_members as public_class_or_enum_missing_members,
    language_public_constructor_body_exception as public_constructor_body_exception,
    language_public_constructor_body_exception_observation as public_constructor_body_exception_observation,
    language_public_constructor_sibling_data_shape_observations as public_constructor_sibling_data_shape_observations,
    language_public_constructor_signature_mismatch as public_constructor_signature_mismatch,
    language_public_exception_mismatch as public_exception_mismatch,
    language_public_expected_exception_not_raised as public_expected_exception_not_raised,
    language_public_method_sibling_obligations as public_method_sibling_obligations,
    language_public_missing_attributes as public_missing_attributes,
    language_public_missing_method_attributes as public_missing_method_attributes,
    language_public_output_stream_assertion_mismatch as public_output_stream_assertion_mismatch,
    language_public_state_assertion_observations as public_state_assertion_observations,
    language_public_state_assertions as public_state_assertions,
    language_public_state_terminal_transition_obligations as public_state_terminal_transition_obligations,
    language_source_import_time_name_resolution_defect as source_import_time_name_resolution_defect,
    language_source_parse_defect as source_parse_defect, language_source_targets_from_text,
    language_verification_repair_authority_target,
};
use crate::session::{
    ContractReconciliationDiagnostic, DocsPendingDeliverable, DocsRouteState, FailureKind,
    ProcessPhase, RepairControlSnapshotDiagnostic, RepairIntentDiagnostic, RepairLaneDiagnostic,
    RepairOperationTemplate, RepairRecoveryChoiceDiagnostic, SessionStateSnapshot, TaskRoute,
    VerificationFailureCluster, VerificationFailureEvidence,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepairLaneProjection {
    pub subtype: RepairLaneSubtype,
    pub required_target: Option<String>,
    pub allowed_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
    pub missing_symbol: Option<String>,
    pub public_state_assertions: Vec<String>,
    pub public_missing_attributes: Vec<String>,
    pub contract_reconciliation: Option<ContractReconciliationDiagnostic>,
    pub operation_template: Option<RepairOperationTemplate>,
    pub verification_cluster: Option<VerificationFailureCluster>,
    pub repair_intent: Option<RepairIntentDiagnostic>,
    pub repair_control_snapshot: Option<RepairControlSnapshotDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RepairLaneSubtype {
    DocsRouteContractRepair,
    GeneratedTestSubprocessEncodingMissing,
    GeneratedTestSubprocessOutputCaptureMissing,
    GeneratedTestLoggingContractOverreach,
    GeneratedTestParseDefect,
    GeneratedTestArtifactApiMisuse,
    ImportExportMissingExport,
    NoTestsRan,
    PublicClassAttributeMismatch,
    PublicConstructorBodyException,
    PublicConstructorSignatureMismatch,
    PublicCallableSignatureMismatch,
    PublicExceptionMismatch,
    PublicMethodAttributeMismatch,
    PublicMissingAttributeMismatch,
    PublicCommandContractFailure,
    PublicOutputStreamAssertionMismatch,
    PublicStateAssertionMismatch,
    SourceImportTimeNameResolution,
    SourceParseDefect,
    PatchMismatch,
    GenericVerificationFailure,
}

impl RepairLaneSubtype {
    fn as_str(&self) -> &'static str {
        match self {
            Self::DocsRouteContractRepair => "docs_route_contract_repair",
            Self::GeneratedTestSubprocessEncodingMissing => {
                "generated_test_subprocess_encoding_missing"
            }
            Self::GeneratedTestSubprocessOutputCaptureMissing => {
                "generated_test_subprocess_output_capture_missing"
            }
            Self::GeneratedTestLoggingContractOverreach => {
                "generated_test_logging_contract_overreach"
            }
            Self::GeneratedTestParseDefect => "generated_test_parse_defect",
            Self::GeneratedTestArtifactApiMisuse => "generated_test_artifact_api_misuse",
            Self::ImportExportMissingExport => "import_export_missing_export",
            Self::NoTestsRan => "no_tests_ran",
            Self::PublicClassAttributeMismatch => "public_class_attribute_mismatch",
            Self::PublicConstructorBodyException => "public_constructor_body_exception",
            Self::PublicConstructorSignatureMismatch => "public_constructor_signature_mismatch",
            Self::PublicCallableSignatureMismatch => "public_callable_signature_mismatch",
            Self::PublicExceptionMismatch => "public_exception_mismatch",
            Self::PublicMethodAttributeMismatch => "public_method_attribute_mismatch",
            Self::PublicMissingAttributeMismatch => "public_missing_attribute_mismatch",
            Self::PublicCommandContractFailure => "public_command_contract_failure",
            Self::PublicOutputStreamAssertionMismatch => "public_output_stream_assertion_mismatch",
            Self::PublicStateAssertionMismatch => "public_state_assertion_mismatch",
            Self::SourceImportTimeNameResolution => "source_import_time_name_resolution",
            Self::SourceParseDefect => "source_parse_defect",
            Self::PatchMismatch => "patch_mismatch",
            Self::GenericVerificationFailure => "generic_verification_failure",
        }
    }
}

impl RepairLaneProjection {
    pub(crate) fn diagnostic(&self) -> RepairLaneDiagnostic {
        RepairLaneDiagnostic {
            subtype: self.subtype.as_str().to_string(),
            required_target: self.required_target.clone(),
            allowed_tools: self.allowed_tools.clone(),
            forbidden_tools: self.forbidden_tools.clone(),
            missing_symbol: self.missing_symbol.clone(),
            public_state_assertions: self.public_state_assertions.clone(),
            public_missing_attributes: self.public_missing_attributes.clone(),
            contract_reconciliation: self.contract_reconciliation.clone(),
            operation_template: self.operation_template.clone(),
            verification_cluster: self.verification_cluster.clone(),
            repair_intent: self.repair_intent.clone(),
            repair_control_snapshot: self.repair_control_snapshot.clone(),
        }
    }
}

pub(crate) fn project_repair_lane(
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
) -> Option<RepairLaneProjection> {
    if !state.completion.verification_pending
        || !matches!(
            state.process_phase,
            ProcessPhase::Verify | ProcessPhase::Repair
        )
    {
        return None;
    }
    let failure = state.failure.as_ref()?;
    if !matches!(
        failure.kind,
        FailureKind::VerificationFailed | FailureKind::PatchMismatch
    ) {
        return None;
    }

    let verification_cluster = verification_failure_cluster(state);
    if let Some(projection) =
        docs_route_contract_repair_projection(state, allowed_tools, verification_cluster.clone())
    {
        return Some(projection);
    }
    let subtype = repair_lane_subtype(failure.kind, verification_cluster.as_ref());
    let typed_required_target =
        required_target_for_subtype(state, &subtype, verification_cluster.as_ref());
    let mut required_target = typed_required_target.clone();
    if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestLoggingContractOverreach
    ) {
        if let Some(test_target) = first_test_target(&state.active_targets) {
            required_target = Some(test_target.clone());
        }
    }
    let allowed = allowed_tools.iter().cloned().collect::<Vec<_>>();
    let forbidden = forbidden_tools_for_projection(&allowed);
    let missing_symbol = missing_symbol_from_cluster(verification_cluster.as_ref());
    let public_state_assertions =
        public_state_assertions_from_cluster(verification_cluster.as_ref());
    let public_missing_attributes =
        public_missing_attributes_from_cluster(verification_cluster.as_ref());
    let generated_test_target = first_test_target(&state.active_targets);
    let contract_reconciliation =
        reconcile_session_state_failure_with_cluster(state, verification_cluster.as_ref());
    if let Some(reconciliation) = contract_reconciliation.as_ref() {
        if reconciliation
            .required_target
            .as_deref()
            .is_some_and(target_is_test_like)
            && reconciliation.permits_generated_test_repair()
        {
            required_target = reconciliation.required_target.clone();
        } else if reconciliation.owner == ContractFailureOwner::SourceTestContractMismatch {
            required_target = reconciliation.required_target.clone();
        } else if reconciliation.blocks_source_repair() {
            required_target = reconciliation.required_target.clone();
        }
    }
    required_target = normalize_source_owned_required_target(
        required_target,
        state,
        verification_cluster.as_ref(),
        generated_test_target.as_deref(),
        contract_reconciliation.as_ref(),
    );
    let repair_intent = repair_intent_projection(
        &subtype,
        required_target.as_deref(),
        generated_test_target.as_deref(),
        &allowed,
        missing_symbol.as_deref(),
        &public_state_assertions,
        &public_missing_attributes,
        verification_cluster.as_ref(),
        contract_reconciliation.as_ref(),
    );
    let operation_template = repair_operation_template(
        &subtype,
        required_target.as_deref(),
        generated_test_target.as_deref(),
        &allowed,
        &forbidden,
        &public_state_assertions,
        &public_missing_attributes,
        verification_cluster.as_ref(),
        repair_intent.as_ref(),
        contract_reconciliation.as_ref(),
    );
    let repair_control_snapshot = repair_control_snapshot_projection(
        &subtype,
        required_target.as_deref(),
        &allowed,
        &forbidden,
        repair_intent.as_ref(),
        operation_template.as_ref(),
        verification_cluster.as_ref(),
    );
    Some(RepairLaneProjection {
        subtype,
        required_target,
        allowed_tools: allowed,
        forbidden_tools: forbidden,
        missing_symbol,
        public_state_assertions,
        public_missing_attributes,
        contract_reconciliation: contract_reconciliation.map(|decision| decision.diagnostic()),
        operation_template,
        verification_cluster,
        repair_intent,
        repair_control_snapshot,
    })
}

fn docs_route_contract_repair_projection(
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    verification_cluster: Option<VerificationFailureCluster>,
) -> Option<RepairLaneProjection> {
    if !state.completion.route_contract_pending || state.docs_route.is_none() {
        return None;
    }
    let required_target = docs_route_required_target(state)?;
    let subtype = RepairLaneSubtype::DocsRouteContractRepair;
    let allowed = allowed_tools.iter().cloned().collect::<Vec<_>>();
    let forbidden = forbidden_tools_for_projection(&allowed);
    let repair_intent = repair_intent_projection(
        &subtype,
        Some(required_target.as_str()),
        None,
        &allowed,
        None,
        &[],
        &[],
        verification_cluster.as_ref(),
        None,
    );
    let operation_template = repair_operation_template(
        &subtype,
        Some(required_target.as_str()),
        None,
        &allowed,
        &forbidden,
        &[],
        &[],
        verification_cluster.as_ref(),
        repair_intent.as_ref(),
        None,
    );
    let repair_control_snapshot = repair_control_snapshot_projection(
        &subtype,
        Some(required_target.as_str()),
        &allowed,
        &forbidden,
        repair_intent.as_ref(),
        operation_template.as_ref(),
        verification_cluster.as_ref(),
    );
    Some(RepairLaneProjection {
        subtype,
        required_target: Some(required_target),
        allowed_tools: allowed,
        forbidden_tools: forbidden,
        missing_symbol: None,
        public_state_assertions: Vec::new(),
        public_missing_attributes: Vec::new(),
        contract_reconciliation: None,
        operation_template,
        verification_cluster,
        repair_intent,
        repair_control_snapshot,
    })
}

fn docs_route_required_target(state: &SessionStateSnapshot) -> Option<String> {
    let docs = state.docs_route.as_ref()?;
    docs.pending_deliverables
        .iter()
        .map(|item| item.target.as_str().to_string())
        .next()
        .or_else(|| {
            docs.active_deliverable
                .as_ref()
                .map(|target| target.as_str().to_string())
        })
        .or_else(|| {
            docs.deliverables
                .first()
                .map(|deliverable| deliverable.target.as_str().to_string())
        })
}

fn active_targets_contain_repair_target(state: &SessionStateSnapshot, target: &str) -> bool {
    let normalized_target = normalize_target_identity(target);
    state.active_targets.iter().any(|active| {
        let normalized_active = normalize_target_identity(active.as_str());
        normalized_active == normalized_target
    })
}

fn workflow_source_public_operation_cluster(
    cluster_id: &str,
    source_target: &str,
    test_target: &str,
) -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: cluster_id.to_string(),
        failing_labels: vec!["workflow_public_operation".to_string()],
        primary_failure: Some("workflow public operation missing".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("workflow_public_operation".to_string()),
            target: Some(source_target.to_string()),
            symbol: Some("run_workflow".to_string()),
            call_site: Some("run_workflow(sample)".to_string()),
            exception: Some("public operation missing".to_string()),
            expected: Some("workflow result".to_string()),
            observed: Some("run_workflow is missing".to_string()),
            public_state_assertions: vec!["run_workflow(sample)".to_string()],
            public_missing_attributes: vec!["run_workflow".to_string()],
            evidence_markers: vec![
                "workflow-source-contract".to_string(),
                "public_class_attribute_mismatch".to_string(),
            ],
            sibling_obligations: vec![
                "`run_workflow` is missing".to_string(),
                "run_workflow(sample)".to_string(),
            ],
            requirement_refs: vec!["workflow-source-contract".to_string()],
            source_refs: vec![source_target.to_string()],
            test_refs: vec![test_target.to_string()],
        }],
        sibling_obligations: vec![
            "`run_workflow` is missing".to_string(),
            "run_workflow(sample)".to_string(),
        ],
        source_refs: vec![source_target.to_string()],
        test_refs: vec![test_target.to_string()],
    }
}

pub(crate) fn source_owned_verification_repair_lane_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: workflow-source-contract public operation is missing".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_public_operation".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-repair-lane-source-owned-workflow".to_string(),
        failing_labels: vec!["workflow_public_operation".to_string()],
        primary_failure: Some("workflow public operation missing".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("workflow_public_operation".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("run_workflow".to_string()),
            call_site: Some("run_workflow(sample)".to_string()),
            exception: Some("public operation missing".to_string()),
            expected: Some("workflow result".to_string()),
            observed: Some("run_workflow is missing".to_string()),
            public_state_assertions: vec!["run_workflow(sample)".to_string()],
            public_missing_attributes: vec!["run_workflow".to_string()],
            evidence_markers: vec![
                "workflow-source-contract".to_string(),
                "public_class_attribute_mismatch".to_string(),
            ],
            sibling_obligations: vec![
                "`run_workflow` is missing".to_string(),
                "run_workflow(sample)".to_string(),
            ],
            requirement_refs: vec!["workflow-source-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec![
            "`run_workflow` is missing".to_string(),
            "run_workflow(sample)".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.required_target.as_deref() == Some("src/workflow.rs")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceViolatesContract"
                    && !decision.strict_contract_active
                    && decision.source_repair_allowed
                    && !decision.test_repair_allowed
            })
        && snapshot.repair_owner == "source"
        && snapshot.required_target.as_deref() == Some("src/workflow.rs")
        && snapshot.selected_recovery_action == "targeted_edit_then_exact_verification"
        && !snapshot.selected_recovery_action.starts_with("fail_closed")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template.exact_target.as_deref() == Some("src/workflow.rs")
                    && template
                        .verification_rerun_condition
                        .as_deref()
                        .is_some_and(|condition| {
                            condition.contains("recorded verification command")
                        })
            })
}

pub(crate) fn source_config_repair_lane_preserves_common_repair_authority_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("package.json")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: package script contract mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-config-repair-lane-authority".to_string(),
        failing_labels: vec!["package script contract".to_string()],
        primary_failure: Some("npm test failed after package script config change".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("package script contract".to_string()),
            target: Some("package.json".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("package scripts expose test command".to_string()),
            observed: Some("missing test script".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: Vec::new(),
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["package.json".to_string()],
            test_refs: Vec::new(),
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["package.json".to_string()],
        test_refs: Vec::new(),
    });
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    projection.required_target.as_deref() == Some("package.json")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("package.json")
        && projection
            .repair_control_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.required_target.as_deref())
            == Some("package.json")
}

pub(crate) fn docs_route_pending_verification_failure_projects_docs_repair_lane_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.completion.route_contract_pending = true;
    state.completion.verification_pending = true;
    state.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
        pending_deliverables: vec![DocsPendingDeliverable {
            target: Utf8PathBuf::from("docs/workflow-design.md"),
            summary: "same-document docs update remains pending".to_string(),
        }],
        ..DocsRouteState::default()
    });
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: workflow source diagnostic mentioned in docs route output".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    });
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-docs-route-workflow-source-pollution".to_string(),
        failing_labels: vec!["docs route semantic check".to_string()],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: None,
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: None,
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: Vec::new(),
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: Vec::new(),
    });

    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "write".to_string(),
    ]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.subtype == RepairLaneSubtype::DocsRouteContractRepair
        && projection.required_target.as_deref() == Some("docs/workflow-design.md")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template.operation_kind == "docs_route_contract_repair"
                    && template.source_test_ownership == "docs_route"
            })
        && snapshot.repair_owner == "docs_route"
        && snapshot.selected_recovery_action == "targeted_docs_edit_then_exact_verification"
        && snapshot
            .hard_invariants
            .iter()
            .any(|item| item == "forbid_source_or_test_repair_while_docs_route_pending")
}

pub(crate) fn source_owned_repair_lane_rejects_diagnostic_label_targets_fixture_passes() -> bool {
    let label_target = "REQ-workflow-status: public behavior assertion message";
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: workflow public behavior assertion"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_public_status".to_string()];
    let mut cluster = workflow_source_public_operation_cluster(
        "fixture-source-owned-diagnostic-label-workflow",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
    );
    cluster.source_refs = vec![label_target.to_string()];
    cluster.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_state_assertion_mismatch".to_string());
        evidence.target = Some(label_target.to_string());
        evidence.source_refs = vec![label_target.to_string()];
        evidence.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    projection.required_target.as_deref() == Some("src/workflow.rs")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("src/workflow.rs")
        && projection.required_target.as_deref() != Some(label_target)
}

pub(crate) fn source_owned_repair_lane_stays_diagnostic_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: workflow-source-contract public operation is missing".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_public_operation".to_string()];
    state.verification.failure_cluster = Some(workflow_source_public_operation_cluster(
        "fixture-repair-lane-source-owned-diagnostic-workflow",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
    ));
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let allowed_tools = BTreeSet::from(["shell".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    projection.required_target.as_deref() == Some("src/workflow.rs")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.exact_target.as_deref() == Some("src/workflow.rs"))
        && active_targets_contain_repair_target(&state, "src/workflow.rs")
}

pub(crate) fn source_owned_repair_lane_derives_source_from_generated_test_target_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.contract_refs = vec![
        Utf8PathBuf::from("scenario_contract.md"),
        Utf8PathBuf::from("scenario_contract.json"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: public workflow behavior mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_public_behavior".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-owned-generated-test-workflow-target".to_string(),
        failing_labels: vec!["workflow_public_behavior".to_string()],
        primary_failure: Some("workflow public exception behavior was not raised".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_public_behavior".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: Some("expect workflow invalid input to raise public error".to_string()),
            exception: Some("WorkflowError not raised".to_string()),
            expected: Some("WorkflowError".to_string()),
            observed: Some("no public error was raised".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "workflow-source-contract".to_string(),
                "public_exception_mismatch".to_string(),
                "source_public_behavior_assertion".to_string(),
            ],
            sibling_obligations: vec![
                "expected public error `WorkflowError`".to_string(),
                "source public behavior".to_string(),
            ],
            requirement_refs: vec!["workflow-source-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["source public behavior".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("src/workflow.ts")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("src/workflow.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.source_test_ownership == "source")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceViolatesContract"
                    && decision.source_repair_allowed
                    && !decision.test_repair_allowed
            })
        && snapshot.repair_owner == "source"
        && snapshot.required_target.as_deref() == Some("src/workflow.ts")
}

pub(crate) fn source_owned_repair_lane_canonicalizes_absolute_source_target_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verify-contract --behavior failed: public workflow exception contract mismatch"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![
            Utf8PathBuf::from("C:/workspace/project/src/workflow.rs"),
            Utf8PathBuf::from("src/workflow.rs"),
        ],
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_exception_contract".to_string()];
    let mut cluster = workflow_source_public_operation_cluster(
        "fixture-source-owned-absolute-workflow-target",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
    );
    cluster.source_refs = vec!["src/workflow.rs".to_string()];
    cluster.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_exception_mismatch".to_string());
        evidence.target = Some("C:/workspace/project/src/workflow.rs".to_string());
        evidence.source_refs = vec!["src/workflow.rs".to_string()];
        evidence.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.required_target.as_deref() == Some("src/workflow.rs")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("src/workflow.rs")
        && snapshot.required_target.as_deref() == Some("src/workflow.rs")
}

pub(crate) fn no_tests_ran_missing_generated_test_target_stays_test_owned_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "Command: verify-generated-test --collection\n\nNo generated workflow examples were collected".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("tests/workflow.spec.ts")],
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["verify-generated-test --collection".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-no-generated-workflow-tests-ran-target".to_string(),
        failing_labels: Vec::new(),
        primary_failure: Some("Command: verify-generated-test --collection".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("no_tests_ran".to_string()),
            label: None,
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: None,
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
            sibling_obligations: Vec::new(),
            evidence_markers: vec![
                "no_tests_ran".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });
    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template.operation_kind == "generated_test_command_or_collection"
                    && template.source_test_ownership.contains("generated_test")
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && snapshot.repair_owner.contains("generated_test")
}

pub(crate) fn public_output_stream_assertion_mismatch_fixture_passes() -> bool {
    let item = VerificationFailureEvidence {
        evidence_kind: "verification_failure".to_string(),
        subtype: Some("public_output_stream_assertion_mismatch".to_string()),
        label: Some("workflow_cli_invalid_option".to_string()),
        target: None,
        symbol: None,
        call_site: Some("workflow_cli emits stderr diagnostic".to_string()),
        exception: None,
        expected: Some("error event".to_string()),
        observed: Some("empty stderr".to_string()),
        public_state_assertions: Vec::new(),
        public_missing_attributes: Vec::new(),
        evidence_markers: vec![
            "public_output_stream:stderr".to_string(),
            "workflow-source-contract".to_string(),
        ],
        sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
        requirement_refs: vec!["workflow-source-contract".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    };
    item.subtype.as_deref() == Some("public_output_stream_assertion_mismatch")
        && item.target.is_none()
        && item.call_site.as_deref() == Some("workflow_cli emits stderr diagnostic")
        && item.expected.as_deref() == Some("error event")
        && item.observed.as_deref() == Some("empty stderr")
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "public_output_stream:stderr")
        && item
            .sibling_obligations
            .iter()
            .any(|obligation| obligation == "source_public_behavior_assertion")
        && item.source_refs.is_empty()
        && item.test_refs == vec!["tests/workflow.behavior.md".to_string()]
}

fn workflow_generated_test_evidence(
    subtype: &str,
    label: &str,
    observed: &str,
    markers: Vec<String>,
) -> VerificationFailureEvidence {
    VerificationFailureEvidence {
        evidence_kind: "verification_failure".to_string(),
        subtype: Some(subtype.to_string()),
        label: Some(label.to_string()),
        target: Some("tests/workflow.spec.ts".to_string()),
        symbol: None,
        call_site: None,
        exception: None,
        expected: None,
        observed: Some(observed.to_string()),
        public_state_assertions: Vec::new(),
        public_missing_attributes: Vec::new(),
        evidence_markers: markers,
        sibling_obligations: Vec::new(),
        requirement_refs: vec!["workflow-generated-test-contract".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    }
}

fn workflow_generated_test_cluster(
    cluster_id: &str,
    label: &str,
    evidence: VerificationFailureEvidence,
    command: &str,
) -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: cluster_id.to_string(),
        failing_labels: vec![label.to_string()],
        primary_failure: Some(format!("Command: {command}")),
        evidence: vec![evidence],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    }
}

fn workflow_public_command_contract_cluster() -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: "fixture-public-command-contract-failure".to_string(),
        failing_labels: vec!["workflow public argv contract".to_string()],
        primary_failure: Some(
            "public_command_contract_failed: target=src/workflow.rs; observed=argv invocation entered interactive stdin mode instead of processing command-line arguments; expected=direct argv command handling preserves route-owned exit/stdout/stderr contract".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_command_contract_failure".to_string()),
            label: Some("workflow public argv contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: Some("verify-public-command --argv".to_string()),
            exception: None,
            expected: Some(
                "route-owned public argv command satisfies expected exit code and stdout/stderr observation".to_string(),
            ),
            observed: Some(
                "argv invocation entered interactive stdin mode instead of processing command-line arguments".to_string(),
            ),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_command_contract_failure".to_string(),
                "source_public_command_contract_assertion".to_string(),
                "workflow-public-command-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-public-command-contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: Vec::new(),
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: Vec::new(),
    }
}

fn workflow_generated_contract_overreach_cluster(
    cluster_id: &str,
    label: &str,
    subtype: &str,
    primary_failure: &str,
    observed: &str,
    expected: Option<&str>,
    markers: Vec<String>,
) -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: cluster_id.to_string(),
        failing_labels: vec![label.to_string()],
        primary_failure: Some(primary_failure.to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some(subtype.to_string()),
            label: Some(label.to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: Some(label.to_string()),
            exception: None,
            expected: expected.map(str::to_string),
            observed: Some(observed.to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: markers,
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-generated-test-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    }
}

pub(crate) fn generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes()
-> bool {
    let item = workflow_generated_test_evidence(
        "generated_test_subprocess_output_capture_missing",
        "workflow generated-test captures subprocess output",
        "CompletedProcess output stream was not captured for generated-test assertion",
        vec![
            "generated_test_subprocess_output_capture_missing".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    );
    let cluster = workflow_generated_test_cluster(
        "fixture-generated-test-subprocess-capture",
        "workflow generated-test captures subprocess output",
        item.clone(),
        "verify-generated-test --subprocess",
    );
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "generated-test subprocess output capture contract failed".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed = ["write", "apply_patch"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let Some(projection) = project_repair_lane(&state, &allowed) else {
        return false;
    };
    item.subtype.as_deref() == Some("generated_test_subprocess_output_capture_missing")
        && item.target.as_deref() == Some("tests/workflow.spec.ts")
        && item.source_refs.is_empty()
        && item.test_refs == vec!["tests/workflow.spec.ts".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_subprocess_output_capture_missing")
        && item.sibling_obligations.is_empty()
        && projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template.operation_kind == "generated_test_subprocess_output_capture_repair"
                    && template.source_test_ownership.contains("generated_test")
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.repair_owner.contains("generated_test"))
}

pub(crate) fn generated_test_subprocess_encoding_missing_projects_test_repair_fixture_passes()
-> bool {
    let item = workflow_generated_test_evidence(
        "generated_test_subprocess_encoding_missing",
        "workflow generated-test uses explicit subprocess text encoding",
        "child UTF-8 output authority was not declared for generated-test subprocess assertion",
        vec![
            "generated_test_subprocess_encoding_missing".to_string(),
            "generated test subprocess child encoding missing".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    );
    let cluster = workflow_generated_test_cluster(
        "fixture-generated-test-subprocess-encoding",
        "workflow generated-test uses explicit subprocess text encoding",
        item.clone(),
        "verify-generated-test --subprocess",
    );
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "generated-test subprocess encoding contract failed".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed = ["write", "apply_patch"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let Some(projection) = project_repair_lane(&state, &allowed) else {
        return false;
    };
    item.subtype.as_deref() == Some("generated_test_subprocess_encoding_missing")
        && item.target.as_deref() == Some("tests/workflow.spec.ts")
        && item.source_refs.is_empty()
        && item.test_refs == vec!["tests/workflow.spec.ts".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_subprocess_encoding_missing")
        && item
            .observed
            .as_deref()
            .is_some_and(|observed| observed.contains("child UTF-8 output authority"))
        && projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template.operation_kind == "generated_test_subprocess_encoding_repair"
                    && template.source_test_ownership.contains("generated_test")
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.repair_owner.contains("generated_test"))
}

pub(crate) fn generated_test_parse_defect_projects_test_repair_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated workflow test artifact has parse defect"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow generated-test parse contract".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-parse-defect".to_string(),
        failing_labels: vec!["workflow generated-test parse contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --parse".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow generated-test parse contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("generated test parse defect: missing block terminator".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `generated test parse defect: missing block terminator`"
                    .to_string(),
                "source parse frame `tests/workflow.spec.ts`".to_string(),
                "source_parse_defect".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-generated-test-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });
    state
        .verification
        .required_commands
        .push("verify-generated-test --parse".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.source_test_ownership.contains("generated_test"))
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
}

pub(crate) fn generated_test_import_nameerror_projects_test_repair_fixture_passes() -> bool {
    let item = workflow_generated_test_evidence(
        "generated_test_artifact_name_resolution_defect",
        "workflow generated-test has local unresolved fixture symbol",
        "generated test missing name `CONTRACT_FIXTURE`",
        vec![
            "generated_test_artifact_name_resolution_defect".to_string(),
            "generated test missing name `CONTRACT_FIXTURE`".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    );
    let cluster = workflow_generated_test_cluster(
        "fixture-generated-test-import-nameerror",
        "workflow generated-test has local unresolved fixture symbol",
        item.clone(),
        "verify-generated-test --api",
    );
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "generated-test local name resolution defect".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };

    item.test_refs == vec!["tests/workflow.spec.ts".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_artifact_name_resolution_defect")
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker.contains("generated test missing name `CONTRACT_FIXTURE`"))
        && projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.source_test_ownership.contains("generated_test"))
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
}

pub(crate) fn generated_test_reflection_api_misuse_projects_test_repair_fixture_passes() -> bool {
    let item = workflow_generated_test_evidence(
        "generated_test_artifact_api_misuse",
        "workflow generated-test uses invalid reflection subject",
        "generated test invalid reflection subject `module-name-string`",
        vec![
            "generated_test_artifact_api_misuse".to_string(),
            "generated test invalid reflection subject".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    );
    let cluster = workflow_generated_test_cluster(
        "fixture-generated-test-reflection-api-misuse",
        "workflow generated-test uses invalid reflection subject",
        item.clone(),
        "verify-generated-test --api",
    );
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "generated-test invalid reflection subject".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };

    item.test_refs == vec!["tests/workflow.spec.ts".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_artifact_api_misuse")
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker.contains("generated test invalid reflection subject"))
        && projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
}

pub(crate) fn generated_test_module_attribute_api_misuse_projects_test_repair_fixture_passes()
-> bool {
    let item = workflow_generated_test_evidence(
        "generated_test_artifact_api_misuse",
        "workflow generated-test uses invalid module attribute",
        "generated test invalid module attribute `runtime.environment`",
        vec![
            "generated_test_artifact_api_misuse".to_string(),
            "generated test invalid module attribute `runtime.environment`".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    );
    let cluster = workflow_generated_test_cluster(
        "fixture-generated-test-module-attribute-api-misuse",
        "workflow generated-test uses invalid module attribute",
        item.clone(),
        "verify-generated-test --api",
    );
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "generated-test invalid module attribute".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };

    item.subtype.as_deref() == Some("generated_test_artifact_api_misuse")
        && item.public_missing_attributes.is_empty()
        && item.test_refs == vec!["tests/workflow.spec.ts".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_artifact_api_misuse")
        && item.evidence_markers.iter().any(|marker| {
            marker.contains("generated test invalid module attribute `runtime.environment`")
        })
        && projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
}

pub(crate) fn repair_intent_defers_verification_command_evidence_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated workflow test artifact has parse defect"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["verify-generated-test --parse".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-deferred-verification-command-evidence".to_string(),
        failing_labels: vec!["workflow generated-test parse contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --parse".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow generated-test parse contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("generated test parse defect: missing block terminator".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `generated test parse defect: missing block terminator`"
                    .to_string(),
                "source parse frame `tests/workflow.spec.ts`".to_string(),
                "source_parse_defect".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-generated-test-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(intent) = projection.repair_intent.as_ref() else {
        return false;
    };
    let Some(template_intent) = projection
        .operation_template
        .as_ref()
        .and_then(|template| template.repair_intent.as_ref())
    else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    let command_is_absent = |evidence: &[String]| {
        evidence.iter().all(|item| {
            !item.starts_with("Command:") && !item.contains("verify-generated-test --parse")
        })
    };

    command_is_absent(&intent.required_evidence)
        && command_is_absent(&intent.progress_evidence)
        && command_is_absent(&template_intent.required_evidence)
        && command_is_absent(&template_intent.progress_evidence)
        && snapshot.recovery_choices.iter().all(|choice| {
            command_is_absent(&choice.required_evidence)
                && command_is_absent(&choice.progress_evidence)
        })
        && snapshot
            .verification_rerun_condition
            .as_deref()
            .is_some_and(|condition| {
                condition.contains("after a successful edit")
                    && condition.contains("rerun the recorded verification command")
            })
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-generated-test --parse")
}

pub(crate) fn public_command_contract_failure_projects_compact_source_repair_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "public_command_contract_failed: target=src/workflow.rs; observed=argv invocation entered interactive stdin mode instead of processing command-line arguments; expected=direct argv command handling preserves route-owned exit/stdout/stderr contract".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["verify-public-command --argv".to_string()];
    state.verification.failure_cluster = Some(workflow_public_command_contract_cluster());

    let allowed_tools = BTreeSet::from(["apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(template) = projection.operation_template.as_ref() else {
        return false;
    };
    let Some(intent) = projection.repair_intent.as_ref() else {
        return false;
    };
    projection.subtype == RepairLaneSubtype::PublicCommandContractFailure
        && projection.required_target.as_deref() == Some("src/workflow.rs")
        && projection
            .forbidden_tools
            .iter()
            .any(|tool| tool == "write")
        && template.operation_kind == "source_public_command_contract"
        && template.source_test_ownership == "source"
        && template
            .forbidden_stale_tools
            .iter()
            .any(|tool| tool == "write")
        && intent.repair_owner == "source"
        && intent
            .required_edit_intent
            .contains("direct command-line invocations")
        && intent
            .required_evidence
            .iter()
            .any(|item| item.contains("public command contract failure"))
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence
                        .requirement_refs
                        .iter()
                        .any(|reference| reference == "workflow-public-command-contract")
                })
            })
        && intent
            .progress_evidence
            .iter()
            .any(|item| item.contains("content-changing `apply_patch` to `src/workflow.rs`"))
        && intent.progress_evidence.iter().all(|item| {
            !item.contains("`write` or `apply_patch`") && !item.contains("`apply_patch` or `write`")
        })
        && intent.required_evidence.iter().all(|item| {
            !item.contains("runtime frame") && !item.contains("C:\\") && !item.contains("line 9")
        })
        && intent
            .forbidden_directions
            .iter()
            .any(|item| item == "interactive_stdin_only_cli_when_argv_contract_exists")
}

pub(crate) fn generated_test_contract_overreach_projects_test_repair_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: generated test requires an uncontracted side-effect assertion"
                .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels =
        vec!["workflow generated-test contract overreach".to_string()];
    state.verification.failure_cluster = Some(workflow_generated_contract_overreach_cluster(
        "fixture-generated-test-contract-overreach",
        "workflow generated-test contract overreach",
        "generated_test_logging_contract_overreach",
        "generated test asserted an uncontracted side-effect",
        "generated test asserted side-effect evidence outside workflow-generated-test-contract",
        Some("visible public result contract without generated-test-only side effect"),
        vec![
            "generated_test_logging_contract_overreach".to_string(),
            "generated_test_contract_overreach".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    ));
    state
        .verification
        .required_commands
        .push("verify-generated-test --contract".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(template) = projection.operation_template.as_ref() else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && template.exact_target.as_deref() == Some("tests/workflow.spec.ts")
        && template.operation_kind == "generated_test_logging_contract_repair"
        && template.source_test_ownership.contains("generated_test")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
                    && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test asserts uncontracted public output"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels =
        vec!["workflow generated-test public output contract".to_string()];
    state.verification.failure_cluster = Some(workflow_generated_contract_overreach_cluster(
        "fixture-ungrounded-generated-public-output",
        "workflow generated-test public output contract",
        "generic_verification_failure",
        "generated test asserted public output outside workflow-generated-test-contract",
        "current public output omits the generated-test-only literal",
        Some("scenario-visible public output contract"),
        vec![
            "generated_test_contract_overreach".to_string(),
            "generated-test public output formatting assertion overreach".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    ));
    state
        .verification
        .required_commands
        .push("verify-generated-test --output".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence
                        .evidence_markers
                        .iter()
                        .any(|marker| marker == "generated_test_contract_overreach")
                })
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
                    && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
}

pub(crate) fn generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test asserts uncontracted public output format"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels =
        vec!["workflow generated-test public output formatting contract".to_string()];
    state.verification.failure_cluster = Some(workflow_generated_contract_overreach_cluster(
        "fixture-generated-test-public-output-format-overreach",
        "workflow generated-test public output formatting contract",
        "generic_verification_failure",
        "generated test asserted public output formatting outside workflow-generated-test-contract",
        "public output exposed compact result form",
        Some("scenario-visible result line"),
        vec![
            "generated_test_contract_overreach".to_string(),
            "generated-test public output formatting assertion overreach".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    ));
    state
        .verification
        .required_commands
        .push("verify-generated-test --output".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence.observed.as_deref().is_some_and(|observed| {
                        observed.contains("compact result form") && !observed.contains("unmatched")
                    }) && evidence
                        .evidence_markers
                        .iter()
                        .any(|marker| marker == "generated_test_contract_overreach")
                })
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
                    && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn generated_test_exception_type_overreach_projects_test_repair_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test asserts uncontracted exception type"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels =
        vec!["workflow generated-test exception contract".to_string()];
    state.verification.failure_cluster = Some(workflow_generated_contract_overreach_cluster(
        "fixture-generated-test-exception-type-overreach",
        "workflow generated-test exception contract",
        "generic_verification_failure",
        "generated test asserted exact exception taxonomy outside workflow-generated-test-contract",
        "source exposed a contract-compliant error classification",
        Some("scenario-visible error classification"),
        vec![
            "generated_test_contract_overreach".to_string(),
            "generated-test exception type assertion overreach".to_string(),
            "workflow-generated-test-contract".to_string(),
        ],
    ));
    state
        .verification
        .required_commands
        .push("verify-generated-test --exception".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence.expected.as_deref() == Some("scenario-visible error classification")
                        && evidence.observed.as_deref()
                            == Some("source exposed a contract-compliant error classification")
                        && evidence
                            .evidence_markers
                            .iter()
                            .any(|marker| marker == "generated_test_contract_overreach")
                })
            })
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
                    && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated workflow test stale literal".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels =
        vec!["workflow generated-test visible contract".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generic-generated-test-only-repair-lane".to_string(),
        failing_labels: vec!["workflow generated-test visible contract".to_string()],
        primary_failure: Some("generated-test visible contract assertion drift".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow generated-test visible contract".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("old visible literal".to_string()),
            observed: Some("current visible literal".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-generated-test-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });
    state
        .verification
        .required_commands
        .push("verify-generated-test --contract".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(template) = projection.operation_template.as_ref() else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && template.exact_target.as_deref() == Some("tests/workflow.spec.ts")
        && template.operation_kind == "source_test_contract_repair"
        && template.source_test_ownership == "source_or_generated_test_by_contract_evidence"
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceTestContractMismatch"
                    && decision.source_repair_allowed
                    && decision.test_repair_allowed
                    && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
        && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && snapshot.repair_owner == "source_or_generated_test_by_contract_evidence"
}

pub(crate) fn contract_visible_public_exception_projects_source_repair_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.contract_refs = vec![
        Utf8PathBuf::from("scenario_contract.md"),
        Utf8PathBuf::from("scenario_contract.json"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public exception was not raised".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_public_behavior".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-contract-visible-public-exception-repair".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("workflow public exception behavior was not raised".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_public_behavior".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: Some("expect workflow invalid input to raise public error".to_string()),
            exception: Some("WorkflowError not raised".to_string()),
            expected: Some("WorkflowError".to_string()),
            observed: Some("no public error was raised".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_exception_mismatch".to_string(),
                "source_public_behavior_assertion".to_string(),
                "workflow-source-contract".to_string(),
            ],
            sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
            requirement_refs: vec!["workflow-source-contract".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(template) = projection.operation_template.as_ref() else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("src/workflow.ts")
        && template.exact_target.as_deref() == Some("src/workflow.ts")
        && template.operation_kind == "source_exception_contract"
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceViolatesContract"
                    && decision.source_repair_allowed
                    && !decision.test_repair_allowed
                    && decision.required_target.as_deref() == Some("src/workflow.ts")
            })
        && snapshot.repair_owner == "source"
        && snapshot.required_target.as_deref() == Some("src/workflow.ts")
}

fn repair_lane_subtype(
    kind: FailureKind,
    cluster: Option<&VerificationFailureCluster>,
) -> RepairLaneSubtype {
    if matches!(kind, FailureKind::PatchMismatch) {
        return RepairLaneSubtype::PatchMismatch;
    }
    cluster
        .and_then(|cluster| {
            cluster
                .evidence
                .iter()
                .filter_map(|evidence| evidence.subtype.as_deref())
                .find_map(repair_lane_subtype_from_str)
        })
        .unwrap_or(RepairLaneSubtype::GenericVerificationFailure)
}

fn repair_lane_subtype_from_str(value: &str) -> Option<RepairLaneSubtype> {
    match value {
        "generated_test_subprocess_encoding_missing" => {
            Some(RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing)
        }
        "generated_test_subprocess_output_capture_missing" => {
            Some(RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing)
        }
        "generated_test_logging_contract_overreach" => {
            Some(RepairLaneSubtype::GeneratedTestLoggingContractOverreach)
        }
        "generated_test_parse_defect" => Some(RepairLaneSubtype::GeneratedTestParseDefect),
        "import_export_missing_export" => Some(RepairLaneSubtype::ImportExportMissingExport),
        "no_tests_ran" => Some(RepairLaneSubtype::NoTestsRan),
        "public_class_attribute_mismatch" => Some(RepairLaneSubtype::PublicClassAttributeMismatch),
        "public_constructor_body_exception" => {
            Some(RepairLaneSubtype::PublicConstructorBodyException)
        }
        "public_constructor_signature_mismatch" => {
            Some(RepairLaneSubtype::PublicConstructorSignatureMismatch)
        }
        "public_callable_signature_mismatch" => {
            Some(RepairLaneSubtype::PublicCallableSignatureMismatch)
        }
        "public_exception_mismatch" => Some(RepairLaneSubtype::PublicExceptionMismatch),
        "public_method_attribute_mismatch" => {
            Some(RepairLaneSubtype::PublicMethodAttributeMismatch)
        }
        "public_missing_attribute_mismatch" => {
            Some(RepairLaneSubtype::PublicMissingAttributeMismatch)
        }
        "public_command_contract_failure" => Some(RepairLaneSubtype::PublicCommandContractFailure),
        "public_output_stream_assertion_mismatch" => {
            Some(RepairLaneSubtype::PublicOutputStreamAssertionMismatch)
        }
        "public_state_assertion_mismatch" => Some(RepairLaneSubtype::PublicStateAssertionMismatch),
        "source_import_time_name_resolution" => {
            Some(RepairLaneSubtype::SourceImportTimeNameResolution)
        }
        "source_parse_defect" => Some(RepairLaneSubtype::SourceParseDefect),
        "patch_mismatch" => Some(RepairLaneSubtype::PatchMismatch),
        "generic_verification_failure" => Some(RepairLaneSubtype::GenericVerificationFailure),
        _ => None,
    }
}

fn repair_lane_subtype_from_summary(kind: FailureKind, summary: &str) -> RepairLaneSubtype {
    let lower = summary.to_ascii_lowercase();
    if matches!(kind, FailureKind::PatchMismatch) {
        RepairLaneSubtype::PatchMismatch
    } else if generated_test_subprocess_encoding_missing(summary).is_some() {
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
    } else if generated_test_subprocess_output_capture_missing(summary).is_some() {
        RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
    } else if generated_test_logging_contract_overreach(summary).is_some() {
        RepairLaneSubtype::GeneratedTestLoggingContractOverreach
    } else if generated_test_parse_defect(summary).is_some() {
        RepairLaneSubtype::GeneratedTestParseDefect
    } else if generated_test_module_attribute_api_misuse(summary).is_some()
        || generated_test_reflection_api_misuse(summary).is_some()
    {
        RepairLaneSubtype::GeneratedTestArtifactApiMisuse
    } else if lower.contains("cannot import name") {
        RepairLaneSubtype::ImportExportMissingExport
    } else if lower.contains("no tests ran") {
        RepairLaneSubtype::NoTestsRan
    } else if source_parse_defect(summary).is_some() {
        RepairLaneSubtype::SourceParseDefect
    } else if source_import_time_name_resolution_defect(summary).is_some() {
        RepairLaneSubtype::SourceImportTimeNameResolution
    } else if public_constructor_body_exception(summary).is_some() {
        RepairLaneSubtype::PublicConstructorBodyException
    } else if !public_class_or_enum_missing_members(summary).is_empty() {
        RepairLaneSubtype::PublicClassAttributeMismatch
    } else if public_constructor_signature_mismatch(summary).is_some() {
        RepairLaneSubtype::PublicConstructorSignatureMismatch
    } else if public_callable_signature_mismatch(summary).is_some() {
        RepairLaneSubtype::PublicCallableSignatureMismatch
    } else if public_exception_mismatch(summary).is_some()
        || public_expected_exception_not_raised(summary).is_some()
    {
        RepairLaneSubtype::PublicExceptionMismatch
    } else if !public_missing_method_attributes(summary).is_empty() {
        RepairLaneSubtype::PublicMethodAttributeMismatch
    } else if public_command_contract_failure(summary).is_some() {
        RepairLaneSubtype::PublicCommandContractFailure
    } else if public_output_stream_assertion_mismatch(summary).is_some() {
        RepairLaneSubtype::PublicOutputStreamAssertionMismatch
    } else if (lower.contains("assertionerror:")
        || lower.contains("indexerror: list index out of range")
        || lower.contains("public state assertion mismatch detected"))
        && !public_state_assertions(summary).is_empty()
    {
        RepairLaneSubtype::PublicStateAssertionMismatch
    } else if (lower.contains("attributeerror:")
        || lower.contains("public missing-attribute mismatch detected"))
        && !public_missing_attributes(summary).is_empty()
    {
        RepairLaneSubtype::PublicMissingAttributeMismatch
    } else {
        RepairLaneSubtype::GenericVerificationFailure
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicCommandContractFailure {
    command: Option<String>,
    observed_issue: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GeneratedTestParseDefect {
    detail: String,
    target: Option<String>,
}

fn generated_test_parse_defect(summary: &str) -> Option<GeneratedTestParseDefect> {
    let lower = summary.to_ascii_lowercase();
    let generated_test_marker = lower.contains("generated-test")
        || lower.contains("generated test")
        || lower.contains("generated_test");
    let parse_marker = lower.contains("parse defect")
        || lower.contains("parse-defect")
        || lower.contains("syntax error")
        || lower.contains("syntaxerror");
    if !generated_test_marker || !parse_marker {
        return None;
    }
    let target = summary
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '`' | '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
                )
        })
        .map(|token| token.trim_matches(|ch: char| matches!(ch, ':' | '.' | '!' | '?')))
        .map(|token| token.replace('\\', "/"))
        .filter(|token| classify_artifact_target(token).role == ArtifactRole::Test)
        .next();
    let detail = source_parse_defect(summary)
        .map(|defect| defect.detail)
        .unwrap_or_else(|| "generated-test parse defect".to_string());
    Some(GeneratedTestParseDefect { detail, target })
}

fn public_command_contract_failure(summary: &str) -> Option<PublicCommandContractFailure> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("public_command_contract")
        || lower.contains("public command contract")
        || lower.contains("route-owned public argv command contract"))
    {
        return None;
    }
    let command = failure_summary_logical_lines(summary)
        .into_iter()
        .find_map(|line| {
            line.trim()
                .strip_prefix("command:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let observed_issue = if lower.contains("interactive stdin") || lower.contains("eoferror") {
        "argv invocation entered interactive stdin mode instead of processing command-line arguments"
            .to_string()
    } else if lower.contains("stdout had no line ending")
        || lower.contains("expected public result line suffix")
    {
        "stdout did not expose the expected public result line suffix".to_string()
    } else if lower.contains("stderr contained none")
        || lower.contains("stdout contained none")
        || lower.contains("usage/help/error")
    {
        "stdout/stderr did not expose the expected usage/help/error observation".to_string()
    } else {
        "public command did not satisfy the route-owned argv/exit/stdout/stderr contract"
            .to_string()
    };
    Some(PublicCommandContractFailure {
        command,
        observed_issue,
    })
}

fn required_target_for_subtype(
    state: &SessionStateSnapshot,
    subtype: &RepairLaneSubtype,
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    match subtype {
        RepairLaneSubtype::DocsRouteContractRepair => docs_route_required_target(state),
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
        | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
        | RepairLaneSubtype::GeneratedTestArtifactApiMisuse
        | RepairLaneSubtype::GeneratedTestParseDefect
        | RepairLaneSubtype::GeneratedTestLoggingContractOverreach => {
            first_test_target(&state.active_targets).or_else(|| first_target(&state.active_targets))
        }
        RepairLaneSubtype::ImportExportMissingExport => import_export_source_target(state, cluster)
            .or_else(|| first_non_test_target(&state.active_targets))
            .or_else(|| first_non_test_repair_authority_target(&state.active_targets))
            .or_else(|| first_target(&state.active_targets)),
        RepairLaneSubtype::NoTestsRan => {
            generated_test_repair_target(state).or_else(|| first_target(&state.active_targets))
        }
        RepairLaneSubtype::PublicClassAttributeMismatch
        | RepairLaneSubtype::PublicConstructorBodyException
        | RepairLaneSubtype::PublicConstructorSignatureMismatch
        | RepairLaneSubtype::PublicMissingAttributeMismatch
        | RepairLaneSubtype::PublicCommandContractFailure
        | RepairLaneSubtype::PublicOutputStreamAssertionMismatch
        | RepairLaneSubtype::PublicStateAssertionMismatch
        | RepairLaneSubtype::SourceImportTimeNameResolution
        | RepairLaneSubtype::SourceParseDefect => first_non_test_target(&state.active_targets)
            .or_else(|| first_non_test_repair_authority_target(&state.active_targets))
            .or_else(|| first_non_test_failure_target(state))
            .or_else(|| first_non_test_failure_repair_authority_target(state))
            .or_else(|| first_source_ref_target(cluster))
            .or_else(|| first_repair_authority_source_ref_target(cluster))
            .or_else(|| source_target_from_cluster_test_refs(cluster)),
        RepairLaneSubtype::PublicCallableSignatureMismatch => cluster
            .and_then(|cluster| {
                cluster
                    .evidence
                    .iter()
                    .filter_map(|evidence| evidence.target.clone())
                    .find(|target| target_is_non_test_repair_authority(target))
            })
            .or_else(|| first_non_test_target(&state.active_targets))
            .or_else(|| first_non_test_repair_authority_target(&state.active_targets)),
        RepairLaneSubtype::PublicMethodAttributeMismatch => {
            first_non_test_target(&state.active_targets)
                .or_else(|| first_non_test_repair_authority_target(&state.active_targets))
                .or_else(|| first_non_test_failure_target(state))
                .or_else(|| first_non_test_failure_repair_authority_target(state))
                .or_else(|| first_source_ref_target(cluster))
                .or_else(|| first_repair_authority_source_ref_target(cluster))
        }
        RepairLaneSubtype::PublicExceptionMismatch => first_non_test_target(&state.active_targets)
            .or_else(|| first_non_test_repair_authority_target(&state.active_targets))
            .or_else(|| first_non_test_failure_target(state))
            .or_else(|| first_non_test_failure_repair_authority_target(state))
            .or_else(|| first_source_ref_target(cluster))
            .or_else(|| first_repair_authority_source_ref_target(cluster))
            .or_else(|| {
                cluster.and_then(|cluster| {
                    cluster
                        .evidence
                        .iter()
                        .filter_map(|evidence| evidence.target.clone())
                        .find(|target| target_is_non_test_repair_authority(target))
                })
            }),
        RepairLaneSubtype::PatchMismatch | RepairLaneSubtype::GenericVerificationFailure => {
            first_source_call_site_target(cluster).or_else(|| first_target(&state.active_targets))
        }
    }
}

fn generated_test_repair_target(state: &SessionStateSnapshot) -> Option<String> {
    state
        .failure
        .as_ref()
        .and_then(|failure| first_test_target(&failure.targets))
        .or_else(|| first_test_target(&state.active_targets))
}

fn import_export_source_target(
    state: &SessionStateSnapshot,
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    if let Some(target) = cluster.and_then(|cluster| {
        cluster
            .evidence
            .iter()
            .find_map(|evidence| evidence.target.clone())
    }) {
        return Some(target);
    }
    first_non_test_target(&state.active_targets).or_else(|| first_target(&state.active_targets))
}

fn first_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets.first().map(|target| target.as_str().to_string())
}

fn first_non_test_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .find(|target| target_is_mutable_source_like(target.as_str()))
        .map(|target| target.as_str().to_string())
}

fn first_non_test_repair_authority_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .find(|target| target_is_non_test_repair_authority(target.as_str()))
        .map(|target| target.as_str().to_string())
}

fn first_non_test_failure_target(state: &SessionStateSnapshot) -> Option<String> {
    state
        .failure
        .as_ref()
        .and_then(|failure| first_non_test_target(&failure.targets))
}

fn first_non_test_failure_repair_authority_target(state: &SessionStateSnapshot) -> Option<String> {
    state
        .failure
        .as_ref()
        .and_then(|failure| first_non_test_repair_authority_target(&failure.targets))
}

fn first_source_ref_target(cluster: Option<&VerificationFailureCluster>) -> Option<String> {
    cluster.and_then(|cluster| {
        cluster
            .source_refs
            .iter()
            .chain(
                cluster
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.source_refs.iter()),
            )
            .find(|target| target_is_mutable_source_like(target))
            .cloned()
    })
}

fn first_repair_authority_source_ref_target(
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    cluster.and_then(|cluster| {
        cluster
            .source_refs
            .iter()
            .chain(
                cluster
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.source_refs.iter()),
            )
            .find(|target| target_is_non_test_repair_authority(target))
            .cloned()
    })
}

fn first_test_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .find(|target| target_is_test_like(target.as_str()))
        .map(|target| target.as_str().to_string())
}

fn normalize_source_owned_required_target(
    required_target: Option<String>,
    state: &SessionStateSnapshot,
    cluster: Option<&VerificationFailureCluster>,
    generated_test_target: Option<&str>,
    reconciliation: Option<&ContractReconciliationDecision>,
) -> Option<String> {
    let Some(reconciliation) = reconciliation else {
        return required_target;
    };
    if !reconciliation.source_repair_allowed || reconciliation.test_repair_allowed {
        return required_target;
    }
    if required_target
        .as_deref()
        .is_some_and(target_is_non_test_repair_authority)
    {
        if let Some(target) = required_target.as_deref()
            && let Some(canonical) = canonical_relative_source_target_for(target, state, cluster)
        {
            return Some(canonical);
        }
        return required_target;
    }

    first_non_test_target(&state.active_targets)
        .or_else(|| first_non_test_repair_authority_target(&state.active_targets))
        .or_else(|| first_non_test_failure_target(state))
        .or_else(|| first_non_test_failure_repair_authority_target(state))
        .or_else(|| first_source_ref_target(cluster))
        .or_else(|| first_repair_authority_source_ref_target(cluster))
        .or_else(|| first_source_call_site_target(cluster))
        .or_else(|| {
            required_target
                .as_deref()
                .and_then(source_target_for_generated_test_target)
        })
        .or_else(|| generated_test_target.and_then(source_target_for_generated_test_target))
        .or_else(|| source_target_from_test_targets(&state.active_targets))
        .or_else(|| {
            state
                .failure
                .as_ref()
                .and_then(|failure| source_target_from_test_targets(&failure.targets))
        })
        .or_else(|| source_target_from_cluster_test_refs(cluster))
}

fn first_source_call_site_target(cluster: Option<&VerificationFailureCluster>) -> Option<String> {
    cluster.and_then(|cluster| {
        cluster
            .evidence
            .iter()
            .filter_map(|evidence| evidence.call_site.as_deref())
            .flat_map(language_source_targets_from_text)
            .find(|target| target_is_mutable_source_like(target))
    })
}

fn canonical_relative_source_target_for(
    target: &str,
    state: &SessionStateSnapshot,
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    first_matching_source_target(&state.active_targets, target)
        .or_else(|| {
            state
                .failure
                .as_ref()
                .and_then(|failure| first_matching_source_target(&failure.targets, target))
        })
        .or_else(|| {
            cluster.and_then(|cluster| {
                cluster
                    .source_refs
                    .iter()
                    .chain(
                        cluster
                            .evidence
                            .iter()
                            .flat_map(|evidence| evidence.source_refs.iter()),
                    )
                    .find(|candidate| source_targets_equivalent(candidate, target))
                    .cloned()
            })
        })
}

fn first_matching_source_target(targets: &[Utf8PathBuf], target: &str) -> Option<String> {
    targets
        .iter()
        .map(|candidate| candidate.as_str())
        .find(|candidate| {
            target_is_mutable_source_like(candidate) && source_targets_equivalent(candidate, target)
        })
        .map(str::to_string)
}

fn source_targets_equivalent(candidate: &str, target: &str) -> bool {
    normalize_target_identity(candidate).eq_ignore_ascii_case(&normalize_target_identity(target))
}

fn normalize_target_identity(target: &str) -> String {
    let normalized = target.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(normalized.as_str())
        .trim_end_matches('/')
        .to_string()
}

fn source_target_from_test_targets(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .filter_map(|target| source_target_for_generated_test_target(target.as_str()))
        .next()
}

fn source_target_from_cluster_test_refs(
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    cluster.and_then(|cluster| {
        cluster
            .test_refs
            .iter()
            .chain(
                cluster
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.test_refs.iter()),
            )
            .filter_map(|target| source_target_for_generated_test_target(target))
            .next()
    })
}

fn source_target_for_generated_test_target(target: &str) -> Option<String> {
    let spec = classify_artifact_target(target);
    (spec.role == ArtifactRole::Test)
        .then_some(spec.source_path)
        .flatten()
        .filter(|source| target_is_mutable_source_like(source))
}

fn target_is_test_like(target: &str) -> bool {
    classify_artifact_target(target).role == ArtifactRole::Test
}

fn target_is_mutable_source_like(target: &str) -> bool {
    let spec = classify_artifact_target(target);
    let file_name = spec
        .normalized_target
        .rsplit('/')
        .next()
        .unwrap_or(spec.normalized_target.as_str())
        .to_ascii_lowercase();
    spec.role == ArtifactRole::Source
        && !matches!(
            file_name.as_str(),
            "scenario_contract.md" | "scenario_contract.json"
        )
        && matches!(spec.language, LanguageFamily::Python | LanguageFamily::Code)
}

fn target_is_non_test_repair_authority(target: &str) -> bool {
    language_verification_repair_authority_target(target) && !target_is_test_like(target)
}

fn missing_import_symbol(summary: &str) -> Option<String> {
    let marker = "cannot import name '";
    let start = summary.find(marker)? + marker.len();
    let rest = &summary[start..];
    let end = rest.find('\'')?;
    let symbol = rest[..end].trim();
    (!symbol.is_empty()).then(|| symbol.to_string())
}

fn import_module_name(summary: &str) -> Option<String> {
    let marker = " from '";
    let start = summary.find(marker)? + marker.len();
    let rest = &summary[start..];
    let end = rest.find('\'')?;
    let module = rest[..end].trim();
    (!module.is_empty()).then(|| module.to_string())
}

pub(crate) fn verification_failure_evidence_from_summary(
    kind: FailureKind,
    summary: &str,
) -> Vec<VerificationFailureEvidence> {
    let subtype = repair_lane_subtype_from_summary(kind, summary);
    let mut evidence_markers = repair_evidence_markers_from_summary(&subtype, summary);
    evidence_markers.extend(contract_classification_markers_from_summary(summary));
    let mut public_state_assertions = public_state_assertions(summary);
    let mut public_missing_attributes = public_missing_attributes(summary);
    if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestArtifactApiMisuse
            | RepairLaneSubtype::GeneratedTestParseDefect
    ) {
        public_missing_attributes.clear();
        public_state_assertions.clear();
    }
    let sibling_obligations = repair_sibling_obligations_from_summary(
        &subtype,
        summary,
        &public_state_assertions,
        &public_missing_attributes,
    );
    evidence_markers.extend(sibling_obligations.iter().cloned());
    evidence_markers.sort();
    evidence_markers.dedup();
    public_state_assertions = stable_unique(public_state_assertions);
    public_missing_attributes = stable_unique(public_missing_attributes);

    let symbol = missing_import_symbol(summary).or_else(|| {
        source_import_time_name_resolution_defect(summary).map(|defect| defect.missing_name)
    });
    let target = typed_evidence_target_from_summary(&subtype, summary);
    let call_site = typed_evidence_call_site_from_summary(&subtype, summary);
    let exception = public_exception_mismatch(summary)
        .map(|mismatch| mismatch.actual_exception)
        .or_else(|| {
            public_constructor_body_exception(summary)
                .map(|observation| observation.actual_exception)
        });
    let source_refs = source_refs_for_evidence(&subtype, summary, target.as_deref());
    let test_refs = test_refs_for_evidence(&subtype, summary, target.as_deref());
    let public_output_mismatch = public_output_stream_assertion_mismatch(summary);
    let public_command_failure = public_command_contract_failure(summary);
    let generated_encoding_missing = generated_test_subprocess_encoding_missing(summary);
    let generated_capture_missing = generated_test_subprocess_output_capture_missing(summary);

    vec![VerificationFailureEvidence {
        evidence_kind: "verification_failure".to_string(),
        subtype: Some(subtype.as_str().to_string()),
        label: None,
        target,
        symbol,
        call_site: call_site.or_else(|| {
            public_output_mismatch
                .as_ref()
                .map(|mismatch| mismatch.assertion_line.clone())
                .or_else(|| {
                    generated_encoding_missing
                        .as_ref()
                        .map(|mismatch| mismatch.assertion_line.clone())
                })
                .or_else(|| {
                    generated_capture_missing
                        .as_ref()
                        .map(|mismatch| mismatch.assertion_line.clone())
                })
        }),
        exception,
        expected: public_output_mismatch
            .as_ref()
            .map(|mismatch| mismatch.expected_substring.clone())
            .or_else(|| {
                public_exception_mismatch(summary)
                    .and_then(|mismatch| mismatch.expected_exception)
            })
            .or_else(|| {
                public_command_failure.as_ref().map(|_| {
                    "route-owned public argv command satisfies expected exit code and stdout/stderr observation".to_string()
                })
            })
            .or_else(|| {
                generated_encoding_missing
                    .as_ref()
                    .map(|mismatch| mismatch.expected_substring.clone())
            })
            .or_else(|| {
                generated_capture_missing
                    .as_ref()
                    .map(|mismatch| mismatch.expected_substring.clone())
            }),
        observed: typed_evidence_observed_from_summary(&subtype, summary).or_else(|| {
            public_state_assertion_observations(summary)
                .into_iter()
                .next()
        }),
        public_state_assertions,
        public_missing_attributes,
        evidence_markers,
        sibling_obligations,
        requirement_refs: Vec::new(),
        source_refs,
        test_refs,
    }]
}

fn typed_evidence_observed_from_summary(
    subtype: &RepairLaneSubtype,
    summary: &str,
) -> Option<String> {
    match subtype {
        RepairLaneSubtype::SourceParseDefect => {
            source_parse_defect(summary).map(|defect| defect.detail)
        }
        RepairLaneSubtype::SourceImportTimeNameResolution => {
            source_import_time_name_resolution_defect(summary)
                .map(|defect| format!("missing source name `{}`", defect.missing_name))
        }
        RepairLaneSubtype::PublicConstructorSignatureMismatch => {
            public_constructor_signature_mismatch(summary).map(|mismatch| mismatch.detail)
        }
        RepairLaneSubtype::PublicCallableSignatureMismatch => {
            public_callable_signature_mismatch(summary).map(|mismatch| mismatch.detail)
        }
        RepairLaneSubtype::PublicExceptionMismatch => {
            public_exception_mismatch(summary).map(|mismatch| mismatch.actual_exception)
        }
        RepairLaneSubtype::PublicConstructorBodyException => {
            public_constructor_body_exception(summary)
                .map(|observation| observation.actual_exception)
        }
        RepairLaneSubtype::PublicOutputStreamAssertionMismatch => {
            public_output_stream_assertion_mismatch(summary)
                .map(|mismatch| mismatch.observed_output)
        }
        RepairLaneSubtype::PublicCommandContractFailure => {
            public_command_contract_failure(summary).map(|failure| failure.observed_issue)
        }
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing => {
            generated_test_subprocess_encoding_missing(summary)
                .map(|mismatch| mismatch.observed_output)
        }
        RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing => {
            generated_test_subprocess_output_capture_missing(summary)
                .map(|mismatch| mismatch.observed_output)
        }
        RepairLaneSubtype::GeneratedTestParseDefect => generated_test_parse_defect(summary)
            .map(|defect| format!("generated test parse defect `{}`", defect.detail)),
        RepairLaneSubtype::GeneratedTestArtifactApiMisuse => {
            generated_test_module_attribute_api_misuse(summary)
                .map(|defect| {
                    format!(
                        "generated test invalid module attribute `{}`",
                        defect.missing_name
                    )
                })
                .or_else(|| {
                    generated_test_reflection_api_misuse(summary).map(|defect| {
                        format!(
                            "generated test invalid reflection subject `{}`",
                            defect.missing_name
                        )
                    })
                })
        }
        _ => None,
    }
}

fn typed_evidence_target_from_summary(
    subtype: &RepairLaneSubtype,
    summary: &str,
) -> Option<String> {
    match subtype {
        RepairLaneSubtype::ImportExportMissingExport => {
            import_module_name(summary).map(|module| format!("{}.py", module.replace('.', "/")))
        }
        RepairLaneSubtype::SourceParseDefect => {
            source_parse_defect(summary).and_then(|defect| defect.path)
        }
        RepairLaneSubtype::SourceImportTimeNameResolution => {
            source_import_time_name_resolution_defect(summary).and_then(|defect| defect.path)
        }
        RepairLaneSubtype::PublicCallableSignatureMismatch => {
            public_callable_signature_mismatch(summary).and_then(|mismatch| mismatch.source_target)
        }
        RepairLaneSubtype::PublicExceptionMismatch => {
            public_exception_mismatch(summary).and_then(|mismatch| mismatch.source_site)
        }
        RepairLaneSubtype::PublicConstructorBodyException
        | RepairLaneSubtype::PublicConstructorSignatureMismatch
        | RepairLaneSubtype::PublicClassAttributeMismatch
        | RepairLaneSubtype::PublicMethodAttributeMismatch
        | RepairLaneSubtype::PublicMissingAttributeMismatch
        | RepairLaneSubtype::PublicCommandContractFailure
        | RepairLaneSubtype::PublicOutputStreamAssertionMismatch
        | RepairLaneSubtype::PublicStateAssertionMismatch => {
            source_refs_from_summary(summary).into_iter().next()
        }
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
        | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
        | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
        | RepairLaneSubtype::GeneratedTestParseDefect
        | RepairLaneSubtype::GeneratedTestArtifactApiMisuse
        | RepairLaneSubtype::NoTestsRan => {
            test_refs_for_subtype(subtype, summary).into_iter().next()
        }
        RepairLaneSubtype::PatchMismatch
        | RepairLaneSubtype::DocsRouteContractRepair
        | RepairLaneSubtype::GenericVerificationFailure => None,
    }
}

fn typed_evidence_call_site_from_summary(
    subtype: &RepairLaneSubtype,
    summary: &str,
) -> Option<String> {
    match subtype {
        RepairLaneSubtype::PublicConstructorSignatureMismatch => {
            public_constructor_signature_mismatch(summary).and_then(|mismatch| mismatch.call_site)
        }
        RepairLaneSubtype::PublicConstructorBodyException => {
            public_constructor_body_exception(summary).map(|observation| {
                observation
                    .source_failure_site
                    .unwrap_or(observation.constructor_call_site)
            })
        }
        RepairLaneSubtype::PublicCallableSignatureMismatch => {
            public_callable_signature_mismatch(summary).and_then(|mismatch| mismatch.call_site)
        }
        RepairLaneSubtype::PublicExceptionMismatch => {
            public_exception_mismatch(summary).and_then(|mismatch| mismatch.call_site)
        }
        RepairLaneSubtype::PublicCommandContractFailure => {
            public_command_contract_failure(summary).and_then(|failure| failure.command)
        }
        RepairLaneSubtype::GeneratedTestArtifactApiMisuse => {
            generated_test_module_attribute_api_misuse(summary)
                .map(|defect| defect.missing_name)
                .or_else(|| {
                    generated_test_reflection_api_misuse(summary).map(|defect| defect.missing_name)
                })
        }
        _ => None,
    }
}

fn source_refs_from_summary(summary: &str) -> Vec<String> {
    language_file_refs_from_summary(summary, ArtifactRole::Source)
}

fn source_refs_for_evidence(
    subtype: &RepairLaneSubtype,
    summary: &str,
    target: Option<&str>,
) -> Vec<String> {
    let mut refs = source_refs_from_summary(summary);
    if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
            | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
            | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
            | RepairLaneSubtype::GeneratedTestParseDefect
            | RepairLaneSubtype::GeneratedTestArtifactApiMisuse
    ) {
        return Vec::new();
    }
    if matches!(subtype, RepairLaneSubtype::ImportExportMissingExport)
        && let Some(target) = target
        && target_is_mutable_source_like(target)
        && !refs
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(target))
    {
        refs.insert(0, target.to_string());
    }
    stable_unique(refs)
}

fn test_refs_from_summary(summary: &str) -> Vec<String> {
    language_file_refs_from_summary(summary, ArtifactRole::Test)
}

fn test_refs_for_evidence(
    subtype: &RepairLaneSubtype,
    summary: &str,
    target: Option<&str>,
) -> Vec<String> {
    let mut refs = test_refs_for_subtype(subtype, summary);
    if matches!(subtype, RepairLaneSubtype::GeneratedTestParseDefect)
        && let Some(target) = target
        && target_is_test_like(target)
        && !refs
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(target))
    {
        refs.insert(0, target.to_string());
    }
    stable_unique(refs)
}

fn test_refs_for_subtype(subtype: &RepairLaneSubtype, summary: &str) -> Vec<String> {
    if matches!(subtype, RepairLaneSubtype::GeneratedTestParseDefect) {
        let mut refs = generated_test_parse_defect(summary)
            .and_then(|defect| defect.target)
            .map(|target| vec![target])
            .unwrap_or_default();
        refs.extend(test_refs_from_summary(summary));
        return stable_unique(refs);
    }
    test_refs_from_summary(summary)
}

fn contract_classification_markers_from_summary(summary: &str) -> Vec<String> {
    let lower = summary.to_ascii_lowercase();
    let mut markers = Vec::new();
    if lower.contains("providercapabilitymismatch")
        || lower.contains("provider capability mismatch")
        || lower.contains("vision metadata mismatch")
        || lower.contains("image_count mismatch")
        || lower.contains("image part missing despite vision metadata")
    {
        markers.push("provider_capability_mismatch".to_string());
    }
    if lower.contains("harness invariant")
        || lower.contains("repaircontrolsnapshot")
        || lower.contains("toolresult feedback")
        || lower.contains("request diagnostics")
    {
        markers.push("harness_invariant_violation".to_string());
    }
    if lower.contains("toolorenvironmentfailure")
        || lower.contains("tool or environment failure")
        || lower.contains("python not found")
        || lower.contains("docling unavailable")
        || lower.contains("filesystem error")
        || lower.contains("shell environment failure")
    {
        markers.push("tool_or_environment_failure".to_string());
    }
    if lower.contains("oracleconflict")
        || lower.contains("oracle conflict")
        || lower.contains("contract/gate conflict")
        || lower.contains("scenario contract and generated test disagree")
        || lower.contains("harness-owned gate and generated test disagree")
    {
        markers.push("oracle_conflict".to_string());
    }
    if lower.contains("generatedtestinsufficient")
        || lower.contains("generated test insufficient")
        || lower.contains("insufficient generated test coverage")
        || lower.contains("generated test does not cover")
    {
        markers.push("generated_test_insufficient".to_string());
    }
    if lower.contains("contractoutofscope")
        || lower.contains("contract out of scope")
        || lower.contains("out-of-scope public")
        || lower.contains("not listed in scenario_contract")
        || lower.contains("not listed in the scenario contract")
        || lower.contains("generatedtestoutofscope")
    {
        markers.push("generated_test_out_of_scope".to_string());
    }
    if generated_test_name_resolution_defect(summary).is_some() {
        markers.push("generated_test_artifact_name_resolution_defect".to_string());
    }
    if generated_test_subprocess_encoding_missing(summary).is_some() {
        markers.push("generated_test_subprocess_encoding_missing".to_string());
    }
    if generated_test_subprocess_output_capture_missing(summary).is_some() {
        markers.push("generated_test_subprocess_output_capture_missing".to_string());
    }
    if generated_test_parse_defect(summary).is_some() {
        markers.push("generated_test_artifact_parse_defect".to_string());
        markers.push("generated_test_parse_defect".to_string());
    }
    if generated_test_public_output_contract_overreach(summary).is_some() {
        markers.push("generated_test_contract_overreach".to_string());
    }
    if generated_test_exception_type_overreach(summary).is_some() {
        markers.push("generated_test_contract_overreach".to_string());
    }
    markers
}

fn missing_symbol_from_cluster(cluster: Option<&VerificationFailureCluster>) -> Option<String> {
    cluster.and_then(|cluster| {
        cluster
            .evidence
            .iter()
            .find_map(|evidence| evidence.symbol.clone())
    })
}

fn public_state_assertions_from_cluster(
    cluster: Option<&VerificationFailureCluster>,
) -> Vec<String> {
    stable_unique(
        cluster
            .into_iter()
            .flat_map(|cluster| cluster.evidence.iter())
            .flat_map(|evidence| evidence.public_state_assertions.iter().cloned())
            .collect(),
    )
}

fn public_missing_attributes_from_cluster(
    cluster: Option<&VerificationFailureCluster>,
) -> Vec<String> {
    stable_unique(
        cluster
            .into_iter()
            .flat_map(|cluster| cluster.evidence.iter())
            .flat_map(|evidence| evidence.public_missing_attributes.iter().cloned())
            .collect(),
    )
}

fn cluster_evidence_markers(cluster: Option<&VerificationFailureCluster>) -> Vec<String> {
    sorted_unique(
        cluster
            .into_iter()
            .flat_map(|cluster| cluster.evidence.iter())
            .flat_map(|evidence| evidence.evidence_markers.iter().cloned())
            .collect(),
    )
}

fn cluster_sibling_obligations(cluster: Option<&VerificationFailureCluster>) -> Vec<String> {
    sorted_unique(
        cluster
            .into_iter()
            .flat_map(|cluster| {
                cluster.sibling_obligations.iter().cloned().chain(
                    cluster
                        .evidence
                        .iter()
                        .flat_map(|evidence| evidence.sibling_obligations.iter().cloned()),
                )
            })
            .collect(),
    )
}

fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn stable_unique(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn has_explicit_generated_test_conflict_evidence(summary: &str) -> bool {
    let normalized = summary.to_ascii_lowercase();
    (normalized.contains("generated test setup contradicts")
        || normalized.contains("generated-test setup contradicts")
        || normalized.contains("generated test data model contradicts")
        || normalized.contains("generated-test data model contradicts")
        || normalized.contains("generated test contradicts")
        || normalized.contains("generated-test contradicts"))
        && (normalized.contains("already-read") || normalized.contains("already read"))
}

fn public_constructor_sibling_obligations(summary: &str) -> Vec<String> {
    let mut obligations = public_constructor_sibling_data_shape_observations(summary);
    if let Some(observation) = public_constructor_body_exception_observation(summary) {
        obligations.extend(observation.sibling_constructor_obligations);
        if let Some(site) = observation.source_failure_site {
            obligations.push(format!("source constructor body failure site `{site}`"));
        }
        obligations.push(format!(
            "constructor body raised `{}`",
            observation.actual_exception
        ));
    }
    obligations.sort();
    obligations.dedup();
    obligations
}

fn public_constructor_signature_markers(summary: &str) -> Vec<String> {
    let Some(mismatch) = public_constructor_signature_mismatch(summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        format!("{}.__init__()", mismatch.constructor),
        mismatch.detail,
    ];
    if let Some(keyword) = mismatch.unexpected_keyword {
        markers.push(format!("unexpected keyword `{keyword}`"));
    }
    if let Some(call_site) = mismatch.call_site {
        markers.push(format!("constructor call site `{call_site}`"));
    }
    markers
}

fn failure_summary_logical_lines(summary: &str) -> Vec<&str> {
    summary
        .lines()
        .flat_map(|line| line.split('|'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn forbidden_tools_for_projection(allowed: &[String]) -> Vec<String> {
    let mut forbidden = Vec::new();
    for tool in ["apply_patch", "read", "shell", "todowrite", "write"] {
        if !allowed.iter().any(|allowed_tool| allowed_tool == tool) {
            forbidden.push(tool.to_string());
        }
    }
    forbidden
}

fn repair_operation_template(
    subtype: &RepairLaneSubtype,
    required_target: Option<&str>,
    generated_test_target: Option<&str>,
    allowed_tools: &[String],
    forbidden_tools: &[String],
    public_state_assertions: &[String],
    public_missing_attributes: &[String],
    cluster: Option<&VerificationFailureCluster>,
    repair_intent: Option<&RepairIntentDiagnostic>,
    contract_reconciliation: Option<&ContractReconciliationDecision>,
) -> Option<RepairOperationTemplate> {
    if contract_reconciliation.is_some_and(ContractReconciliationDecision::fail_closed) {
        return None;
    }
    let target = required_target.map(str::to_string);
    let operation_kind = if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
            | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
            | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
            | RepairLaneSubtype::GeneratedTestParseDefect
            | RepairLaneSubtype::NoTestsRan
    ) {
        repair_operation_kind(subtype)
    } else {
        contract_reconciliation
            .and_then(repair_operation_kind_from_contract_reconciliation)
            .unwrap_or_else(|| repair_operation_kind(subtype))
    }
    .to_string();
    let source_test_ownership = contract_reconciliation
        .and_then(repair_source_test_ownership_from_contract_reconciliation)
        .unwrap_or_else(|| {
            repair_source_test_ownership(subtype, target.as_deref(), generated_test_target)
        })
        .to_string();
    let required_edit_surface = allowed_tools
        .iter()
        .filter(|tool| *tool == "write" || *tool == "apply_patch")
        .cloned()
        .collect::<Vec<_>>();
    let sibling_obligations = repair_sibling_obligations(
        subtype,
        public_state_assertions,
        public_missing_attributes,
        cluster,
    );
    let evidence_markers = repair_evidence_markers(subtype, cluster);
    let operation_id = format!(
        "{}:{}:{}",
        subtype.as_str(),
        target.as_deref().unwrap_or("no_target"),
        stable_short_hash(&format!(
            "{}|{}",
            cluster
                .map(|cluster| cluster.cluster_id.as_str())
                .unwrap_or("no_cluster"),
            allowed_tools.join(",")
        ))
    );

    Some(RepairOperationTemplate {
        operation_id,
        operation_kind,
        exact_target: target,
        source_test_ownership,
        required_edit_surface,
        forbidden_stale_tools: forbidden_tools.to_vec(),
        verification_rerun_condition: Some(
            "after a successful edit to the exact target, rerun the recorded verification command"
                .to_string(),
        ),
        evidence_markers,
        sibling_obligations,
        repair_intent: repair_intent.cloned(),
    })
}

fn repair_intent_projection(
    subtype: &RepairLaneSubtype,
    required_target: Option<&str>,
    generated_test_target: Option<&str>,
    allowed_tools: &[String],
    missing_symbol: Option<&str>,
    public_state_assertions: &[String],
    public_missing_attributes: &[String],
    cluster: Option<&VerificationFailureCluster>,
    contract_reconciliation: Option<&ContractReconciliationDecision>,
) -> Option<RepairIntentDiagnostic> {
    let fail_closed_without_target =
        contract_reconciliation.is_some_and(ContractReconciliationDecision::fail_closed);
    if required_target.is_none() && !fail_closed_without_target {
        return None;
    }
    let exact_target = required_target.unwrap_or("no_repair_target");
    let target_evidence = format!("exact target `{exact_target}`");
    let mut required_evidence = vec![target_evidence];
    if let Some(primary_failure) = cluster.and_then(|cluster| cluster.primary_failure.as_ref()) {
        if !is_deferred_verification_command_evidence(primary_failure) {
            required_evidence.push(primary_failure.clone());
        }
    }
    required_evidence.extend(public_state_assertions.iter().take(4).cloned());
    required_evidence.extend(public_missing_attributes.iter().take(4).cloned());
    if let Some(symbol) = missing_symbol {
        required_evidence.push(format!("missing public symbol `{symbol}`"));
    }
    required_evidence.sort();
    required_evidence.dedup();

    let mut progress_evidence = if fail_closed_without_target {
        vec![
            "no source or generated-test edit is permitted until contract reconciliation is resolved"
                .to_string(),
        ]
    } else {
        vec![format!(
            "content-changing {} to `{exact_target}`",
            edit_progress_surface_label(allowed_tools)
        )]
    };
    if let Some(generated_test_target) = generated_test_target {
        progress_evidence.push(format!(
            "source/test ownership remains explicit against `{generated_test_target}`"
        ));
    }

    let (repair_owner, rollback_depth, recovery_action, required_edit_intent, forbidden) =
        contract_reconciliation
            .and_then(repair_intent_from_contract_reconciliation)
            .unwrap_or_else(|| match subtype {
            RepairLaneSubtype::SourceParseDefect => (
                "source",
                "same_target_repair",
                "targeted_edit_then_exact_verification",
                "repair the source parse defect at the already-grounded location",
                vec![
                    "generated_test_rewrite_for_source_syntax_defect",
                    "stale_read_or_shell_before_targeted_repair",
                ],
            ),
            RepairLaneSubtype::SourceImportTimeNameResolution => (
                "source",
                "same_target_repair",
                "targeted_edit_then_exact_verification",
                "bind, import, rename, or reorder the missing source name",
                vec![
                    "generated_test_rewrite_for_source_import_time_defect",
                    "stale_read_or_shell_before_targeted_repair",
                ],
            ),
            RepairLaneSubtype::GeneratedTestLoggingContractOverreach => (
                "generated_test",
                "generated_test_contract_reconciliation",
                "targeted_test_contract_edit_then_verification",
                "remove or rewrite the generated-test logging side-effect assertion so the test checks the visible stdout/stderr/return-code contract unless logging is explicitly required",
                vec![
                    "source_logging_side_effect_for_test_owned_obligation",
                    "weakening_harness_owned_stdio_gate",
                    "stale_read_or_shell_before_test_contract_repair",
                ],
            ),
            RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing => (
                "generated_test",
                "generated_test_contract_reconciliation",
                "targeted_test_contract_edit_then_verification",
                "add explicit child UTF-8 output authority such as PYTHONUTF8=1 and PYTHONIOENCODING=utf-8, or python -X utf8, before the generated test decodes subprocess output as UTF-8",
                vec![
                    "source_public_output_patch_for_test_owned_encoding_defect",
                    "weakening_stdout_stderr_assertion_without_child_encoding_authority",
                    "stale_read_or_shell_before_test_contract_repair",
                ],
            ),
            RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing => (
                "generated_test",
                "generated_test_contract_reconciliation",
                "targeted_test_contract_edit_then_verification",
                "add subprocess stdout/stderr capture authority to the generated test before asserting CompletedProcess output streams",
                vec![
                    "source_public_output_patch_for_test_owned_capture_defect",
                    "weakening_stdout_stderr_assertion_without_capture",
                    "stale_read_or_shell_before_test_contract_repair",
                ],
            ),
            RepairLaneSubtype::GeneratedTestParseDefect => (
                "generated_test",
                "generated_test_contract_reconciliation",
                "targeted_test_contract_edit_then_verification",
                "repair the generated test parse defect so executable test code can run the public behavior assertion",
                vec![
                    "source_rewrite_for_test_owned_parse_defect",
                    "weakening_generated_test_without_parse_repair",
                    "stale_read_or_shell_before_test_contract_repair",
                ],
            ),
            RepairLaneSubtype::GeneratedTestArtifactApiMisuse => (
                "generated_test",
                "generated_test_contract_reconciliation",
                "targeted_test_contract_edit_then_verification",
                "repair the generated test module/API usage so executable test code asserts only the visible contract",
                vec![
                    "source_public_api_patch_for_test_owned_module_api_misuse",
                    "weakening_source_contract_without_generated_test_conflict",
                    "stale_read_or_shell_before_test_contract_repair",
                ],
            ),
            RepairLaneSubtype::ImportExportMissingExport => (
                "source_or_generated_test_by_contract_evidence",
                "source_test_contract_reconciliation",
                "bounded_replan_then_exact_edit",
                "define/export the required public symbol or reconcile an over-broad generated-test import contract",
                vec![
                    "single_symbol_stub_without_public_contract_check",
                    "generated_test_rewrite_without_contract_conflict_evidence",
                    "stale_read_or_shell_before_contract_repair",
                ],
            ),
            RepairLaneSubtype::PublicExceptionMismatch => (
                "source",
                "same_target_repair",
                "targeted_edit_then_exact_verification",
                "repair the source exception behavior to satisfy the already-read public exception contract",
                vec![
                    "generated_test_rewrite_for_source_exception_defect",
                    "stale_read_or_shell_before_targeted_repair",
                ],
            ),
            RepairLaneSubtype::PublicCallableSignatureMismatch => (
                "source",
                "public_api_contract_repair",
                "bounded_replan_then_exact_edit",
                "repair the source public callable signature exposed by verification",
                vec![
                    "generated_test_rewrite_without_contract_conflict_evidence",
                    "source_signature_replacement_that_breaks_existing_call_sites",
                    "stale_read_or_shell_before_contract_repair",
                ],
            ),
            RepairLaneSubtype::PublicCommandContractFailure => (
                "source",
                "same_target_repair",
                "targeted_edit_then_exact_verification",
                "repair the source CLI argv mode so route-owned public commands do not enter interactive stdin mode and satisfy exit/stdout/stderr observations",
                vec![
                    "generated_test_rewrite_for_source_owned_public_command_violation",
                    "interactive_stdin_only_cli_when_argv_contract_exists",
                    "stale_shell_before_source_contract_repair",
                ],
            ),
            RepairLaneSubtype::DocsRouteContractRepair => (
                "docs_route",
                "same_docs_deliverable_repair",
                "targeted_docs_edit_then_exact_verification",
                "repair the active docs deliverable while preserving the route-owned docs contract authority",
                vec![
                    "source_repair_while_docs_route_pending",
                    "test_rewrite_while_docs_route_pending",
                    "verification_rerun_without_docs_file_change",
                ],
            ),
            RepairLaneSubtype::PublicMissingAttributeMismatch
            | RepairLaneSubtype::PublicOutputStreamAssertionMismatch
            | RepairLaneSubtype::PublicStateAssertionMismatch => (
                "source_or_generated_test_by_contract_evidence",
                "source_test_contract_reconciliation",
                "bounded_replan_then_exact_edit",
                "repair the public behavior contract while preserving exact source/test evidence",
                vec![
                    "test_expectation_weakening_without_contract_conflict_evidence",
                    "local_literal_patch_without_public_contract_reconciliation",
                    "stale_read_or_shell_before_contract_repair",
                ],
            ),
            RepairLaneSubtype::PublicClassAttributeMismatch
            | RepairLaneSubtype::PublicConstructorBodyException
            | RepairLaneSubtype::PublicConstructorSignatureMismatch
            | RepairLaneSubtype::PublicMethodAttributeMismatch => (
                "source",
                "public_api_contract_repair",
                "bounded_replan_then_exact_edit",
                "repair the source public API/data-shape contract exposed by verification",
                vec![
                    "generated_test_rewrite_without_contract_conflict_evidence",
                    "narrow_member_stub_that_drops_sibling_obligations",
                    "stale_read_or_shell_before_contract_repair",
                ],
            ),
            RepairLaneSubtype::NoTestsRan => (
                "verification_command_or_generated_test",
                "verification_command_rebuild",
                "bounded_replan_then_exact_edit_or_rerun",
                "repair test discovery or the recorded verification command before claiming verification",
                vec![
                    "claiming_completion_without_test_collection",
                    "source_rewrite_without_test_discovery_evidence",
                ],
            ),
            RepairLaneSubtype::PatchMismatch => (
                "edit_lifecycle",
                "same_target_repair",
                "exact_edit_or_fail_closed",
                "repair the failed patch/edit lifecycle against the active target",
                vec![
                    "counting_failed_patch_as_progress",
                    "verification_rerun_without_content_changing_edit",
                ],
            ),
            RepairLaneSubtype::GenericVerificationFailure => (
                "unknown",
                "spec_reread_or_failure_registration",
                "fail_closed_until_classified",
                "classify the verification failure into a typed repair operation before further repair",
                vec![
                    "guessing_runtime_branch_from_single_error_string",
                    "weakening_tests_without_contract_evidence",
                ],
            ),
        });
    let mut required_edit_intent = required_edit_intent.to_string();
    let mut forbidden_directions = forbidden
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if matches!(subtype, RepairLaneSubtype::PublicCommandContractFailure) {
        required_edit_intent = "repair the source CLI argv mode so direct command-line invocations satisfy the route-owned public command contract without falling into interactive stdin".to_string();
        required_evidence.push(
            "public command contract failure: argv invocation must produce the recorded exit/stdout/stderr behavior".to_string(),
        );
        required_evidence.push(
            "interactive stdin mode is not a substitute for direct argv command handling"
                .to_string(),
        );
        required_evidence.sort();
        required_evidence.dedup();
        for direction in [
            "generated_test_rewrite_for_source_owned_public_command_violation",
            "interactive_stdin_only_cli_when_argv_contract_exists",
        ] {
            if !forbidden_directions.iter().any(|item| item == direction) {
                forbidden_directions.push(direction.to_string());
            }
        }
    }

    Some(RepairIntentDiagnostic {
        repair_owner: repair_owner.to_string(),
        rollback_depth: rollback_depth.to_string(),
        recovery_action: recovery_action.to_string(),
        required_edit_intent,
        required_evidence,
        progress_evidence,
        forbidden_directions,
    })
}

fn edit_progress_surface_label(allowed_tools: &[String]) -> String {
    let mut edit_tools = allowed_tools
        .iter()
        .filter(|tool| matches!(tool.as_str(), "apply_patch" | "write"))
        .map(|tool| format!("`{tool}`"))
        .collect::<Vec<_>>();
    edit_tools.sort();
    edit_tools.dedup();
    match edit_tools.as_slice() {
        [] => "workspace edit evidence".to_string(),
        [tool] => tool.clone(),
        _ => edit_tools.join(" or "),
    }
}

fn is_deferred_verification_command_evidence(evidence: &str) -> bool {
    evidence.trim_start().starts_with("Command:")
}

fn repair_intent_from_contract_reconciliation(
    decision: &ContractReconciliationDecision,
) -> Option<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Vec<&'static str>,
)> {
    match decision.owner {
        ContractFailureOwner::GeneratedTestOutOfScope => Some((
            "generated_test",
            "generated_test_contract_reconciliation",
            "targeted_test_contract_edit_then_verification",
            "remove or rewrite generated-test assertions that introduce public obligations outside the scenario contract",
            vec![
                "source_api_expansion_for_generated_test_out_of_scope_obligation",
                "harness_contract_update_without_registry_entry",
                "stale_read_or_shell_before_test_contract_repair",
            ],
        )),
        ContractFailureOwner::TestViolatesContract => Some((
            "generated_test",
            "generated_test_contract_reconciliation",
            "targeted_test_contract_edit_then_verification",
            "repair the generated test so it asserts only scenario-contract requirements",
            vec![
                "source_rewrite_for_test_owned_contract_violation",
                "weakening_harness_owned_final_gate",
                "stale_read_or_shell_before_test_contract_repair",
            ],
        )),
        ContractFailureOwner::SourceTestContractMismatch => Some((
            "source_or_generated_test_by_contract_evidence",
            "source_test_contract_reconciliation",
            "bounded_replan_then_exact_edit",
            "repair source behavior where source violates the scenario contract and repair generated-test assertions where the test contradicts that same contract",
            vec![
                "test_expectation_weakening_without_contract_conflict_evidence",
                "source_rewrite_for_test_owned_contract_violation",
                "stale_read_or_shell_before_contract_repair",
            ],
        )),
        ContractFailureOwner::ContractInsufficient => Some((
            "contract",
            "contract_reconciliation_stop",
            "fail_closed_until_contract_owner_classified",
            "register or update the scenario contract before choosing a source or generated-test repair",
            vec![
                "source_repair_without_contract_requirement_id",
                "generated_test_repair_without_contract_requirement_id",
            ],
        )),
        ContractFailureOwner::HarnessInvariantViolation => Some((
            "harness",
            "contract_reconciliation_stop",
            "fail_closed_until_harness_invariant_registered",
            "register the harness invariant failure before source or generated-test repair",
            vec![
                "source_repair_for_harness_invariant_failure",
                "generated_test_repair_for_harness_invariant_failure",
            ],
        )),
        ContractFailureOwner::ProviderCapabilityMismatch => Some((
            "provider_metadata",
            "contract_reconciliation_stop",
            "fail_closed_until_provider_capability_reconciled",
            "fix or register provider/model metadata and request payload capability evidence before source or generated-test repair",
            vec![
                "source_repair_for_provider_capability_mismatch",
                "generated_test_repair_for_provider_capability_mismatch",
            ],
        )),
        ContractFailureOwner::ToolOrEnvironmentFailure => Some((
            "environment",
            "contract_reconciliation_stop",
            "fail_closed_until_tool_or_environment_reconciled",
            "fix or register tool, shell, filesystem, service, or local environment evidence before source or generated-test repair",
            vec![
                "source_repair_for_tool_or_environment_failure",
                "generated_test_repair_for_tool_or_environment_failure",
            ],
        )),
        ContractFailureOwner::OracleConflict => Some((
            "contract",
            "contract_reconciliation_stop",
            "fail_closed_until_oracle_conflict_resolved",
            "resolve the conflict between scenario contract, generated test, and harness-owned gate evidence before source or generated-test repair",
            vec![
                "source_repair_before_oracle_conflict_resolution",
                "generated_test_repair_before_oracle_conflict_resolution",
            ],
        )),
        ContractFailureOwner::GeneratedTestInsufficient => Some((
            "generated_test",
            "contract_reconciliation_report_only",
            "report_generated_test_coverage_gap",
            "record the missing generated-test coverage against scenario contract requirement ids before dispatch policy changes",
            vec!["treating_generated_test_success_as_final_contract_oracle"],
        )),
        ContractFailureOwner::SourceViolatesContract => Some((
            "source",
            "same_target_repair",
            "targeted_edit_then_exact_verification",
            "repair the source behavior that violates the scenario contract requirement ids",
            vec![
                "generated_test_rewrite_for_source_owned_contract_violation",
                "stale_shell_before_source_contract_repair",
                "unbounded_context_churn_before_source_contract_repair",
                "weakening_scenario_contract_without_contract_change_entry",
            ],
        )),
    }
}

fn repair_control_snapshot_projection(
    subtype: &RepairLaneSubtype,
    required_target: Option<&str>,
    allowed_tools: &[String],
    forbidden_tools: &[String],
    repair_intent: Option<&RepairIntentDiagnostic>,
    operation_template: Option<&RepairOperationTemplate>,
    cluster: Option<&VerificationFailureCluster>,
) -> Option<RepairControlSnapshotDiagnostic> {
    let fallback_intent;
    let intent = if let Some(intent) = repair_intent {
        intent
    } else {
        fallback_intent = RepairIntentDiagnostic {
            repair_owner: "unknown".to_string(),
            rollback_depth: "spec_reread_or_failure_registration".to_string(),
            recovery_action: "fail_closed_until_classified".to_string(),
            required_edit_intent:
                "classify the verification failure into a typed repair operation before further repair"
                    .to_string(),
            required_evidence: required_target
                .map(|target| vec![format!("exact target `{target}`")])
                .unwrap_or_else(|| vec!["repair target is not projected".to_string()]),
            progress_evidence: Vec::new(),
            forbidden_directions: vec![
                "guessing_runtime_branch_from_single_error_string".to_string(),
                "dispatching_repair_without_control_snapshot_target".to_string(),
            ],
        };
        &fallback_intent
    };

    let mut hard_invariants = vec![
        "preserve_active_verification_failure".to_string(),
        "target_authority_matches_repair_operation".to_string(),
        "progress_requires_content_changing_edit".to_string(),
        "verification_rerun_requires_current_repair_progress".to_string(),
        "prompt_tool_result_and_request_diagnostics_share_typed_projection".to_string(),
    ];
    match intent.repair_owner.as_str() {
        "source" => hard_invariants
            .push("forbid_generated_test_rewrite_for_source_owned_defect".to_string()),
        "source_or_generated_test_by_contract_evidence" => hard_invariants
            .push("forbid_test_weakening_without_contract_conflict_evidence".to_string()),
        "generated_test" => hard_invariants
            .push("forbid_source_repair_for_generated_test_contract_owner".to_string()),
        "docs_route" => hard_invariants
            .push("forbid_source_or_test_repair_while_docs_route_pending".to_string()),
        "contract" => hard_invariants
            .push("contract_insufficient_must_not_dispatch_source_repair".to_string()),
        "harness" => hard_invariants
            .push("harness_invariant_must_not_dispatch_source_or_test_repair".to_string()),
        "unknown" => hard_invariants
            .push("unclassified_failure_must_fail_closed_before_dispatch".to_string()),
        _ => {}
    }
    if matches!(subtype, RepairLaneSubtype::PatchMismatch) {
        hard_invariants.push("failed_patch_is_not_repair_progress".to_string());
    }
    hard_invariants.sort();
    hard_invariants.dedup();

    let mut recovery_choices = Vec::new();
    recovery_choices.push(RepairRecoveryChoiceDiagnostic {
        recovery_action: intent.recovery_action.clone(),
        rollback_depth: intent.rollback_depth.clone(),
        allowed_tools: allowed_tools.to_vec(),
        required_evidence: intent.required_evidence.clone(),
        forbidden_directions: intent.forbidden_directions.clone(),
        progress_evidence: intent.progress_evidence.clone(),
    });
    if intent.rollback_depth == "source_test_contract_reconciliation" {
        recovery_choices.push(RepairRecoveryChoiceDiagnostic {
            recovery_action: "targeted_edit_then_exact_verification".to_string(),
            rollback_depth: "same_target_repair".to_string(),
            allowed_tools: allowed_tools
                .iter()
                .filter(|tool| *tool == "write" || *tool == "apply_patch")
                .cloned()
                .collect(),
            required_evidence: intent.required_evidence.clone(),
            forbidden_directions: vec![
                "test_expectation_weakening_without_contract_conflict_evidence".to_string(),
                "dropping_source_test_evidence_during_narrow_repair".to_string(),
            ],
            progress_evidence: intent.progress_evidence.clone(),
        });
    }
    recovery_choices.sort_by(|left, right| {
        (left.rollback_depth.as_str(), left.recovery_action.as_str()).cmp(&(
            right.rollback_depth.as_str(),
            right.recovery_action.as_str(),
        ))
    });
    recovery_choices.dedup();

    let mut forbidden_actions = forbidden_tools
        .iter()
        .map(|tool| format!("stale_tool:{tool}"))
        .chain(intent.forbidden_directions.iter().cloned())
        .collect::<Vec<_>>();
    forbidden_actions.sort();
    forbidden_actions.dedup();

    Some(RepairControlSnapshotDiagnostic {
        admitted: true,
        admission_reason: "verification_failure_repair_lane_admitted".to_string(),
        repair_subtype: subtype.as_str().to_string(),
        repair_owner: intent.repair_owner.clone(),
        selected_recovery_action: intent.recovery_action.clone(),
        rollback_depth: intent.rollback_depth.clone(),
        operation_id: operation_template.map(|template| template.operation_id.clone()),
        required_target: required_target.map(str::to_string),
        allowed_surface_snapshot: allowed_tools.to_vec(),
        hard_invariants,
        recovery_choices,
        forbidden_actions,
        progress_evidence: intent.progress_evidence.clone(),
        verification_rerun_condition: operation_template
            .and_then(|template| template.verification_rerun_condition.clone()),
        verification_cluster_id: cluster.map(|cluster| cluster.cluster_id.clone()),
    })
}

fn repair_operation_kind(subtype: &RepairLaneSubtype) -> &'static str {
    match subtype {
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing => {
            "generated_test_subprocess_encoding_repair"
        }
        RepairLaneSubtype::DocsRouteContractRepair => "docs_route_contract_repair",
        RepairLaneSubtype::GeneratedTestLoggingContractOverreach => {
            "generated_test_logging_contract_repair"
        }
        RepairLaneSubtype::GeneratedTestParseDefect => "generated_test_parse_repair",
        RepairLaneSubtype::GeneratedTestArtifactApiMisuse => {
            "generated_test_artifact_api_misuse_repair"
        }
        RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing => {
            "generated_test_subprocess_output_capture_repair"
        }
        RepairLaneSubtype::PublicClassAttributeMismatch => "source_api_member_alias_or_value",
        RepairLaneSubtype::PublicConstructorSignatureMismatch => "source_constructor_signature",
        RepairLaneSubtype::PublicConstructorBodyException => "source_constructor_body",
        RepairLaneSubtype::PublicCallableSignatureMismatch => "source_public_callable_signature",
        RepairLaneSubtype::PublicExceptionMismatch => "source_exception_contract",
        RepairLaneSubtype::PublicMethodAttributeMismatch => "source_public_method_alias",
        RepairLaneSubtype::PublicMissingAttributeMismatch => "source_or_test_data_model",
        RepairLaneSubtype::PublicCommandContractFailure => "source_public_command_contract",
        RepairLaneSubtype::PublicOutputStreamAssertionMismatch => {
            "source_public_output_stream_contract"
        }
        RepairLaneSubtype::PublicStateAssertionMismatch => "source_public_state_invariant",
        RepairLaneSubtype::ImportExportMissingExport => "source_import_export",
        RepairLaneSubtype::SourceImportTimeNameResolution => "source_import_time_name_resolution",
        RepairLaneSubtype::SourceParseDefect => "source_parse_repair",
        RepairLaneSubtype::NoTestsRan => "generated_test_command_or_collection",
        RepairLaneSubtype::PatchMismatch => "patch_mismatch_repair",
        RepairLaneSubtype::GenericVerificationFailure => "generic_verification_repair",
    }
}

fn repair_operation_kind_from_contract_reconciliation(
    decision: &ContractReconciliationDecision,
) -> Option<&'static str> {
    match decision.owner {
        ContractFailureOwner::GeneratedTestOutOfScope
        | ContractFailureOwner::TestViolatesContract => Some("generated_test_contract_repair"),
        ContractFailureOwner::SourceTestContractMismatch => Some("source_test_contract_repair"),
        ContractFailureOwner::ContractInsufficient
        | ContractFailureOwner::HarnessInvariantViolation
        | ContractFailureOwner::ProviderCapabilityMismatch
        | ContractFailureOwner::ToolOrEnvironmentFailure
        | ContractFailureOwner::OracleConflict => Some("contract_reconciliation_stop"),
        ContractFailureOwner::GeneratedTestInsufficient => {
            Some("contract_reconciliation_report_only")
        }
        ContractFailureOwner::SourceViolatesContract => None,
    }
}

fn repair_source_test_ownership(
    subtype: &RepairLaneSubtype,
    required_target: Option<&str>,
    generated_test_target: Option<&str>,
) -> &'static str {
    let targets_generated_test = required_target.is_some()
        && generated_test_target.is_some()
        && required_target == generated_test_target;
    if targets_generated_test
        && matches!(
            subtype,
            RepairLaneSubtype::GeneratedTestLoggingContractOverreach
                | RepairLaneSubtype::ImportExportMissingExport
                | RepairLaneSubtype::PublicMissingAttributeMismatch
                | RepairLaneSubtype::PublicStateAssertionMismatch
        )
    {
        return "generated_test_by_contract_evidence";
    }
    if matches!(subtype, RepairLaneSubtype::DocsRouteContractRepair) {
        return "docs_route";
    }
    if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
            | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
            | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
            | RepairLaneSubtype::GeneratedTestParseDefect
            | RepairLaneSubtype::GeneratedTestArtifactApiMisuse
    ) {
        return "generated_test_by_contract_evidence";
    }
    if matches!(
        subtype,
        RepairLaneSubtype::PublicClassAttributeMismatch
            | RepairLaneSubtype::PublicConstructorBodyException
            | RepairLaneSubtype::PublicConstructorSignatureMismatch
            | RepairLaneSubtype::PublicCallableSignatureMismatch
            | RepairLaneSubtype::PublicExceptionMismatch
            | RepairLaneSubtype::PublicMethodAttributeMismatch
            | RepairLaneSubtype::PublicCommandContractFailure
            | RepairLaneSubtype::PublicOutputStreamAssertionMismatch
            | RepairLaneSubtype::PublicStateAssertionMismatch
            | RepairLaneSubtype::ImportExportMissingExport
            | RepairLaneSubtype::SourceImportTimeNameResolution
            | RepairLaneSubtype::SourceParseDefect
    ) {
        return "source";
    }
    if targets_generated_test {
        "generated_test"
    } else {
        "source_or_generated_test_by_evidence"
    }
}

fn repair_source_test_ownership_from_contract_reconciliation(
    decision: &ContractReconciliationDecision,
) -> Option<&'static str> {
    match decision.owner {
        ContractFailureOwner::GeneratedTestOutOfScope
        | ContractFailureOwner::TestViolatesContract => Some("generated_test_by_scenario_contract"),
        ContractFailureOwner::SourceTestContractMismatch => {
            Some("source_or_generated_test_by_contract_evidence")
        }
        ContractFailureOwner::ContractInsufficient
        | ContractFailureOwner::HarnessInvariantViolation
        | ContractFailureOwner::ProviderCapabilityMismatch
        | ContractFailureOwner::ToolOrEnvironmentFailure
        | ContractFailureOwner::OracleConflict => Some("contract_or_harness_owner"),
        ContractFailureOwner::GeneratedTestInsufficient => {
            Some("generated_test_insufficient_report")
        }
        ContractFailureOwner::SourceViolatesContract => Some("source"),
    }
}

fn repair_evidence_markers(
    subtype: &RepairLaneSubtype,
    cluster: Option<&VerificationFailureCluster>,
) -> Vec<String> {
    let mut markers = Vec::new();
    markers.push(subtype.as_str().to_string());
    markers.extend(cluster_evidence_markers(cluster));
    markers.sort();
    markers.dedup();
    markers
}

fn repair_evidence_markers_from_summary(
    subtype: &RepairLaneSubtype,
    failure_summary: &str,
) -> Vec<String> {
    let mut markers = Vec::new();
    markers.push(subtype.as_str().to_string());
    markers.extend(public_class_member_repair_observations(failure_summary));
    markers.extend(public_constructor_signature_markers(failure_summary));
    if let Some(observation) = public_constructor_body_exception(failure_summary) {
        markers.push(format!(
            "constructor call site `{}`",
            observation.constructor_call_site
        ));
        if let Some(initializer) = observation.source_initializer_call {
            markers.push(format!("constructor initializer `{initializer}`"));
        }
    }
    markers.extend(public_constructor_sibling_obligations(failure_summary));
    if let Some(mismatch) = public_callable_signature_mismatch(failure_summary) {
        markers.push(format!("public callable `{}`", mismatch.callable));
        for arg in mismatch.missing_arguments {
            markers.push(format!("missing callable argument `{arg}`"));
        }
        if let Some(call_site) = mismatch.call_site {
            markers.push(format!("public callable call site `{call_site}`"));
        }
    }
    if let Some(not_raised) = public_expected_exception_not_raised(failure_summary) {
        markers.push("source_public_behavior_assertion".to_string());
        markers.push(format!(
            "expected public exception `{}`",
            not_raised.expected_exception
        ));
        if let Some(call_site) = not_raised.call_site {
            markers.push(format!("public exception assertion `{call_site}`"));
        }
    }
    for method in public_missing_method_attributes(failure_summary) {
        markers.push(format!("public missing method `{}`", method.attribute));
        markers.push(format!("public method call site `{}`", method.call_site));
    }
    markers.extend(public_method_sibling_obligations(failure_summary));
    if !matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
    ) {
        markers.extend(public_output_stream_assertion_markers(failure_summary));
    }
    if let Some(failure) = public_command_contract_failure(failure_summary) {
        markers.push("source_public_command_contract_assertion".to_string());
        markers.push(failure.observed_issue);
        if let Some(command) = failure.command {
            markers.push(format!("public command `{command}`"));
        }
    }
    markers.extend(generated_test_exception_type_overreach_markers(
        failure_summary,
    ));
    markers.extend(public_state_assertions(failure_summary));
    markers.extend(public_state_assertion_observations(failure_summary));
    markers.extend(
        failure_summary_logical_lines(failure_summary)
            .into_iter()
            .filter(|line| {
                line.to_ascii_lowercase()
                    .contains("already-read source context")
            })
            .map(str::to_string),
    );
    markers.extend(public_state_terminal_transition_obligations(
        failure_summary,
    ));
    markers.extend(generated_test_contract_drift_markers_from_summary(
        failure_summary,
    ));
    markers.extend(source_parse_defect_markers(failure_summary));
    markers.extend(source_import_time_name_resolution_markers(failure_summary));
    markers.extend(generated_test_name_resolution_markers(failure_summary));
    markers.extend(generated_test_reflection_api_misuse_markers(
        failure_summary,
    ));
    markers.extend(generated_test_module_attribute_api_misuse_markers(
        failure_summary,
    ));
    markers.extend(generated_test_logging_contract_markers(failure_summary));
    markers.extend(generated_test_parse_defect_markers(failure_summary));
    markers.extend(generated_test_subprocess_output_capture_markers(
        failure_summary,
    ));
    markers.extend(generated_test_subprocess_encoding_markers(failure_summary));
    markers.extend(generated_test_public_output_contract_markers(
        failure_summary,
    ));
    if has_explicit_generated_test_conflict_evidence(failure_summary) {
        markers.push("already-read generated-test conflict evidence".to_string());
    }
    markers.sort();
    markers.dedup();
    markers
}

fn source_parse_defect_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = source_parse_defect(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![format!("source parse defect `{}`", defect.detail)];
    if let Some(path) = defect.path {
        markers.push(format!("source parse frame `{path}`"));
    }
    if let Some(line) = defect.line {
        markers.push(format!("source parse frame line {line}"));
    }
    markers
}

fn generated_test_logging_contract_markers(failure_summary: &str) -> Vec<String> {
    let Some(overreach) = generated_test_logging_contract_overreach(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        "generated-test logging side-effect assertion".to_string(),
        format!(
            "generated-test logging assertion `{}`",
            overreach.assertion_line
        ),
    ];
    if let Some(logger) = overreach.logger_name {
        markers.push(format!("assertLogs logger `{logger}`"));
    }
    if let Some(level) = overreach.level {
        markers.push(format!("assertLogs level `{level}`"));
    }
    markers
}

fn generated_test_parse_defect_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = generated_test_parse_defect(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        "generated_test_artifact_parse_defect".to_string(),
        "generated_test_parse_defect".to_string(),
        format!("generated test parse defect `{}`", defect.detail),
    ];
    if let Some(target) = defect.target {
        markers.push(format!("generated test parse target `{target}`"));
    }
    markers
}

fn generated_test_subprocess_output_capture_markers(failure_summary: &str) -> Vec<String> {
    let Some(mismatch) = generated_test_subprocess_output_capture_missing(failure_summary) else {
        return Vec::new();
    };
    vec![
        "generated_test_subprocess_output_capture_missing".to_string(),
        "generated test subprocess output capture missing".to_string(),
        format!(
            "generated-test subprocess output assertion `{}` expected `{}` observed `{}`",
            mismatch.assertion_line, mismatch.expected_substring, mismatch.observed_value
        ),
    ]
}

fn generated_test_subprocess_encoding_markers(failure_summary: &str) -> Vec<String> {
    let Some(mismatch) = generated_test_subprocess_encoding_missing(failure_summary) else {
        return Vec::new();
    };
    vec![
        "generated_test_subprocess_encoding_missing".to_string(),
        "generated test subprocess child encoding missing".to_string(),
        "generated test parent UTF-8 decode lacks child output encoding authority".to_string(),
        format!(
            "generated-test subprocess output assertion `{}` expected `{}` observed `{}`",
            mismatch.assertion_line, mismatch.expected_substring, mismatch.observed_value
        ),
    ]
}

fn generated_test_public_output_contract_markers(failure_summary: &str) -> Vec<String> {
    let Some(overreach) = generated_test_public_output_contract_overreach(failure_summary) else {
        return Vec::new();
    };
    vec![
        "generated_test_contract_overreach".to_string(),
        "generated-test public output formatting assertion overreach".to_string(),
        format!(
            "generated-test public output assertion `{}` expected `{}` observed `{}`",
            overreach.assertion_line, overreach.expected_substring, overreach.observed_value
        ),
    ]
}

fn generated_test_exception_type_overreach_markers(failure_summary: &str) -> Vec<String> {
    let Some((expected, actual)) = generated_test_exception_type_overreach(failure_summary) else {
        return Vec::new();
    };
    vec![
        "generated_test_contract_overreach".to_string(),
        "generated-test exception type assertion overreach".to_string(),
        format!(
            "generated-test expected exact exception `{expected}` but source raised `{actual}`"
        ),
    ]
}

fn generated_test_exception_type_overreach(failure_summary: &str) -> Option<(String, String)> {
    let mismatch = public_exception_mismatch(failure_summary)?;
    if mismatch.actual_exception.ends_with(" not raised") {
        return None;
    }
    let expected = mismatch.expected_exception?;
    if expected == mismatch.actual_exception {
        return None;
    }
    if test_refs_from_summary(failure_summary).is_empty()
        || source_refs_from_summary(failure_summary).is_empty()
    {
        return None;
    }
    Some((expected, mismatch.actual_exception))
}

fn public_output_stream_assertion_markers(failure_summary: &str) -> Vec<String> {
    let Some(mismatch) = public_output_stream_assertion_mismatch(failure_summary) else {
        return Vec::new();
    };
    vec![
        format!("public_output_stream:{}", mismatch.stream),
        format!(
            "public output stream assertion `{}` contains `{}`",
            mismatch.stream, mismatch.expected_substring
        ),
    ]
}

fn public_output_stream_assertion_obligations(failure_summary: &str) -> Vec<String> {
    let Some(mismatch) = public_output_stream_assertion_mismatch(failure_summary) else {
        return Vec::new();
    };
    vec![
        "source_public_behavior_assertion".to_string(),
        format!(
            "{} contains `{}`",
            mismatch.stream, mismatch.expected_substring
        ),
    ]
}

fn source_import_time_name_resolution_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = source_import_time_name_resolution_defect(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![format!("missing source name `{}`", defect.missing_name)];
    if let Some(suggested_name) = defect.suggested_name {
        markers.push(format!("source near-name candidate `{suggested_name}`"));
    }
    if let Some(path) = defect.path {
        markers.push(format!("source import-time frame `{path}`"));
    }
    if let Some(line) = defect.line {
        markers.push(format!("source import-time frame line {line}"));
    }
    markers
}

fn generated_test_name_resolution_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = generated_test_name_resolution_defect(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        "generated_test_artifact_name_resolution_defect".to_string(),
        format!("generated test missing name `{}`", defect.missing_name),
    ];
    if let Some(suggested_name) = defect.suggested_name {
        markers.push(format!(
            "generated test near-name candidate `{suggested_name}`"
        ));
    }
    if let Some(path) = defect.path {
        markers.push(format!("generated test name-resolution frame `{path}`"));
    }
    if let Some(line) = defect.line {
        markers.push(format!("generated test name-resolution frame line {line}"));
    }
    markers
}

fn generated_test_reflection_api_misuse_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = generated_test_reflection_api_misuse(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        "generated_test_artifact_api_misuse".to_string(),
        format!(
            "generated test invalid reflection subject `{}`",
            defect.missing_name
        ),
    ];
    if let Some(path) = defect.path {
        markers.push(format!("generated test api-misuse frame `{path}`"));
    }
    if let Some(line) = defect.line {
        markers.push(format!("generated test api-misuse frame line {line}"));
    }
    markers
}

fn generated_test_module_attribute_api_misuse_markers(failure_summary: &str) -> Vec<String> {
    let Some(defect) = generated_test_module_attribute_api_misuse(failure_summary) else {
        return Vec::new();
    };
    let mut markers = vec![
        "generated_test_artifact_api_misuse".to_string(),
        format!(
            "generated test invalid module attribute `{}`",
            defect.missing_name
        ),
    ];
    if let Some(path) = defect.path {
        markers.push(format!("generated test api-misuse frame `{path}`"));
    }
    if let Some(line) = defect.line {
        markers.push(format!("generated test api-misuse frame line {line}"));
    }
    markers
}

fn repair_sibling_obligations(
    subtype: &RepairLaneSubtype,
    public_state_assertions: &[String],
    public_missing_attributes: &[String],
    cluster: Option<&VerificationFailureCluster>,
) -> Vec<String> {
    let mut obligations = Vec::new();
    if matches!(subtype, RepairLaneSubtype::PublicClassAttributeMismatch) {
        obligations.extend(cluster_evidence_markers(cluster));
    }
    obligations.extend(public_state_assertions.iter().cloned());
    obligations.extend(public_missing_attributes.iter().cloned());
    obligations.extend(cluster_sibling_obligations(cluster));
    obligations.sort();
    obligations.dedup();
    obligations
}

fn repair_sibling_obligations_from_summary(
    subtype: &RepairLaneSubtype,
    failure_summary: &str,
    public_state_assertions: &[String],
    public_missing_attributes: &[String],
) -> Vec<String> {
    let mut obligations = Vec::new();
    if matches!(subtype, RepairLaneSubtype::PublicClassAttributeMismatch) {
        obligations.extend(public_class_member_repair_observations(failure_summary));
    }
    obligations.extend(public_constructor_signature_markers(failure_summary));
    obligations.extend(public_constructor_sibling_obligations(failure_summary));
    obligations.extend(public_api_data_model_semantic_obligations(failure_summary));
    obligations.extend(public_method_sibling_obligations(failure_summary));
    if let Some(not_raised) = public_expected_exception_not_raised(failure_summary) {
        obligations.push("source_public_behavior_assertion".to_string());
        obligations.push(format!(
            "expected public exception `{}`",
            not_raised.expected_exception
        ));
    }
    if !matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
            | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
    ) {
        obligations.extend(public_output_stream_assertion_obligations(failure_summary));
    }
    obligations.extend(public_state_assertions.iter().cloned());
    obligations.extend(public_state_terminal_transition_obligations(
        failure_summary,
    ));
    obligations.extend(public_missing_attributes.iter().cloned());
    obligations.sort();
    obligations.dedup();
    obligations
}

fn verification_failure_cluster(
    state: &SessionStateSnapshot,
) -> Option<VerificationFailureCluster> {
    state.verification.failure_cluster.clone()
}

pub(crate) fn repair_lane_source_target_identity_exact_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    !active_targets_contain_repair_target(&state, "C:/other/project/src/workflow.rs")
        && !source_targets_equivalent("src/workflow.rs", "C:/other/project/src/workflow.rs")
        && source_targets_equivalent("./src/workflow.rs", "src/workflow.rs")
}

pub(crate) fn repair_lane_public_state_obligations_domain_neutral_fixture_passes() -> bool {
    let assertions = vec!["workflow_state.ready == true".to_string()];
    let obligations = repair_sibling_obligations_from_summary(
        &RepairLaneSubtype::PublicStateAssertionMismatch,
        "AssertionError: workflow state did not satisfy the public state contract",
        &assertions,
        &[],
    );
    let joined = obligations.join("\n").to_ascii_lowercase();
    obligations
        .iter()
        .any(|item| item == "workflow_state.ready == true")
        && [
            "projectile movement delta",
            "projectile bounds lifecycle",
            "projectile spawn coordinate",
            "projectile collision",
            "entity group movement update",
        ]
        .iter()
        .all(|forbidden| !joined.contains(forbidden))
}

pub(crate) fn repair_lane_typed_target_projection_no_required_action_shim_fixture_passes() -> bool {
    contract_visible_public_exception_projects_source_repair_fixture_passes()
        && public_command_contract_failure_projects_compact_source_repair_fixture_passes()
        && repair_lane_source_target_identity_exact_fixture_passes()
}

fn stable_short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

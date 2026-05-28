use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use sha2::{Digest, Sha256};

use crate::agent::content_shape_contract::python_source_for_test_target;
use crate::agent::contract_reconciliation::{
    ContractFailureOwner, ContractReconciliationDecision,
    reconcile_session_state_failure_with_cluster,
};
use crate::session::{
    ContractReconciliationDiagnostic, FailureKind, ProcessPhase, RepairControlSnapshotDiagnostic,
    RepairIntentDiagnostic, RepairLaneDiagnostic, RepairOperationTemplate,
    RepairRecoveryChoiceDiagnostic, SessionStateSnapshot, VerificationFailureCluster,
    VerificationFailureEvidence,
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
    GeneratedTestSubprocessEncodingMissing,
    GeneratedTestSubprocessOutputCaptureMissing,
    GeneratedTestLoggingContractOverreach,
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
            Self::GeneratedTestSubprocessEncodingMissing => {
                "generated_test_subprocess_encoding_missing"
            }
            Self::GeneratedTestSubprocessOutputCaptureMissing => {
                "generated_test_subprocess_output_capture_missing"
            }
            Self::GeneratedTestLoggingContractOverreach => {
                "generated_test_logging_contract_overreach"
            }
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
    let subtype = repair_lane_subtype(failure.kind, verification_cluster.as_ref());
    let typed_required_target =
        required_target_for_subtype(state, &subtype, verification_cluster.as_ref());
    let mut required_target = typed_required_target.clone();
    if typed_repair_target_outranks_required_action(
        &subtype,
        typed_required_target.as_deref(),
        required_target.as_deref(),
        verification_cluster.as_ref(),
    ) && let Some(target) = typed_required_target.as_deref()
    {
        required_target = Some(target.to_string());
    }
    if no_tests_ran_generated_test_target_outranks_stale_write_action(
        &subtype,
        typed_required_target.as_deref(),
    ) && let Some(target) = typed_required_target.as_deref()
    {
        required_target = Some(target.to_string());
    }
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

fn active_targets_contain_repair_target(state: &SessionStateSnapshot, target: &str) -> bool {
    let normalized_target = target.replace('\\', "/");
    state.active_targets.iter().any(|active| {
        let normalized_active = active.as_str().replace('\\', "/");
        normalized_active == normalized_target
            || normalized_active.ends_with(&format!("/{normalized_target}"))
            || normalized_target.ends_with(&format!("/{normalized_active}"))
    })
}

pub(crate) fn source_owned_verification_repair_lane_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: component.calculate is missing".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_calculate_add".to_string()];
    state.verification.failure_cluster =
        Some(crate::agent::state::public_class_attribute_cluster_fixture());
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.required_target.as_deref() == Some("component.py")
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
        && snapshot.selected_recovery_action == "targeted_edit_then_exact_verification"
        && !snapshot.selected_recovery_action.starts_with("fail_closed")
}

pub(crate) fn source_owned_repair_lane_rejects_diagnostic_label_targets_fixture_passes() -> bool {
    let label_target = "BEH-4: bullet overlap assertion message";
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("arcade_game.py"),
        Utf8PathBuf::from("test_arcade_game.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: BEH-4 public behavior assertion".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_update_calls_collision_BEH4".to_string()];
    let mut cluster = crate::agent::state::public_class_attribute_cluster_fixture();
    cluster.source_refs = vec![label_target.to_string()];
    cluster.test_refs = vec!["test_arcade_game.py".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_state_assertion_mismatch".to_string());
        evidence.target = Some(label_target.to_string());
        evidence.source_refs = vec![label_target.to_string()];
        evidence.test_refs = vec!["test_arcade_game.py".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    projection.required_target.as_deref() == Some("arcade_game.py")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("arcade_game.py")
        && projection.required_target.as_deref() != Some(label_target)
}

pub(crate) fn source_owned_repair_lane_stays_diagnostic_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: component.calculate is missing".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_calculate_add".to_string()];
    state.verification.failure_cluster =
        Some(crate::agent::state::public_class_attribute_cluster_fixture());
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["shell".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    projection.required_target.as_deref() == Some("component.py")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.exact_target.as_deref() == Some("component.py"))
        && active_targets_contain_repair_target(&state, "component.py")
}

pub(crate) fn source_owned_repair_lane_derives_source_from_generated_test_target_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public widget behavior mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_cli_no_args".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-owned-generated-test-target".to_string(),
        failing_labels: vec!["test_cli_no_args".to_string()],
        primary_failure: Some("stdout did not match public contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_cli_no_args".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: Some("subprocess.run([sys.executable, 'widget.py'])".to_string()),
            exception: None,
            expected: Some("usage text".to_string()),
            observed: Some("empty stdout".to_string()),
            public_state_assertions: vec!["CLI prints usage text".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["public behavior repair".to_string()],
            sibling_obligations: vec!["widget.py CLI behavior".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["widget.py CLI behavior".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("widget.py")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("widget.py")
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
        && snapshot.required_target.as_deref() == Some("widget.py")
}

pub(crate) fn source_owned_repair_lane_canonicalizes_absolute_source_target_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public widget exception contract mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![
            Utf8PathBuf::from("C:/workspace/project/widget.py"),
            Utf8PathBuf::from("widget.py"),
        ],
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_widget_negative".to_string()];
    let mut cluster = crate::agent::state::public_class_attribute_cluster_fixture();
    cluster.source_refs = vec!["widget.py".to_string()];
    cluster.test_refs = vec!["test_widget.py".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_exception_mismatch".to_string());
        evidence.target = Some("C:/workspace/project/widget.py".to_string());
        evidence.source_refs = vec!["widget.py".to_string()];
        evidence.test_refs = vec!["test_widget.py".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };
    projection.required_target.as_deref() == Some("widget.py")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("widget.py")
        && snapshot.required_target.as_deref() == Some("widget.py")
}

pub(crate) fn no_tests_ran_missing_generated_test_target_stays_test_owned_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "Command: python -X utf8 -m unittest\n\n----------------------------------------------------------------------\nRan 0 tests in 0.000s\n\nOK".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["python -m unittest".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-no-tests-ran-generated-test-target".to_string(),
        failing_labels: Vec::new(),
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
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
            test_refs: vec!["test_widget.py".to_string()],
            sibling_obligations: Vec::new(),
            evidence_markers: vec!["no_tests_ran".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
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
    projection.required_target.as_deref() == Some("test_widget.py")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("test_widget.py")
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
        && snapshot.required_target.as_deref() == Some("test_widget.py")
        && snapshot.repair_owner.contains("generated_test")
}

pub(crate) fn public_output_stream_assertion_mismatch_fixture_passes() -> bool {
    let summary = r#"FAIL: test_cli_invalid_option (test_tool.ToolCliTests.test_cli_invalid_option)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_tool.py", line 17, in test_cli_invalid_option
    self.assertIn("error", result.stderr)
AssertionError: 'error' not found in ''

----------------------------------------------------------------------
Ran 3 tests in 0.120s

FAILED (failures=1)"#;
    let evidence =
        verification_failure_evidence_from_summary(FailureKind::VerificationFailed, summary);
    let Some(item) = evidence.first() else {
        return false;
    };
    item.subtype.as_deref() == Some("public_output_stream_assertion_mismatch")
        && item.target.is_none()
        && item.call_site.as_deref() == Some(r#"self.assertIn("error", result.stderr)"#)
        && item.expected.as_deref() == Some("error")
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
        && item.test_refs == vec!["test_tool.py".to_string()]
}

pub(crate) fn generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes()
-> bool {
    let summary = r#"FAIL: test_interactive_cli (test_calculator.CalculatorTest.test_interactive_cli)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_calculator.py", line 42, in test_interactive_cli
    self.assertIn("結果: 5", result.stdout)
TypeError: argument of type 'NoneType' is not iterable

----------------------------------------------------------------------
Ran 4 tests in 0.130s

FAILED (errors=1)"#;
    let evidence =
        verification_failure_evidence_from_summary(FailureKind::VerificationFailed, summary);
    let Some(item) = evidence.first() else {
        return false;
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-subprocess-capture".to_string(),
        failing_labels: vec!["test_calculator.CalculatorTest.test_interactive_cli".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: evidence.clone(),
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
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
        && item.target.as_deref() == Some("test_calculator.py")
        && item.source_refs.is_empty()
        && item.test_refs == vec!["test_calculator.py".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_subprocess_output_capture_missing")
        && item.sibling_obligations.is_empty()
        && projection.required_target.as_deref() == Some("test_calculator.py")
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
    let summary = r#"Exception in thread Thread-1 (_readerthread):
Traceback (most recent call last):
  File "C:\Python313\Lib\subprocess.py", line 1615, in _readerthread
    buffer.append(fh.read())
  File "<frozen codecs>", line 325, in decode
UnicodeDecodeError: 'utf-8' codec can't decode byte 0x83 in position 0: invalid start byte

======================================================================
ERROR: test_cli_invalid_operator (test_calculator.TestCliEntrypoint.test_cli_invalid_operator)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_calculator.py", line 122, in test_cli_invalid_operator
    self.assertIn('エラー', result.stdout)
TypeError: argument of type 'NoneType' is not iterable

----------------------------------------------------------------------
Ran 17 tests in 0.373s

FAILED (errors=1)"#;
    let evidence =
        verification_failure_evidence_from_summary(FailureKind::VerificationFailed, summary);
    let Some(item) = evidence.first() else {
        return false;
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-subprocess-encoding".to_string(),
        failing_labels: vec![
            "test_calculator.TestCliEntrypoint.test_cli_invalid_operator".to_string(),
        ],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: evidence.clone(),
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
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
        && item.target.as_deref() == Some("test_calculator.py")
        && item.source_refs.is_empty()
        && item.test_refs == vec!["test_calculator.py".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_subprocess_encoding_missing")
        && item.observed.as_deref().is_some_and(|observed| {
            observed.contains("UnicodeDecodeError")
                && observed.contains("child UTF-8 output authority")
        })
        && projection.required_target.as_deref() == Some("test_calculator.py")
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
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: test_widget import failed with SyntaxError".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_widget".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-parse-defect".to_string(),
        failing_labels: vec!["test_widget".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("test_widget".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("SyntaxError: expected ':'".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `SyntaxError: expected ':'`".to_string(),
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
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("test_widget.py")
        && projection
            .operation_template
            .as_ref()
            .and_then(|template| template.exact_target.as_deref())
            == Some("test_widget.py")
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
        && snapshot.required_target.as_deref() == Some("test_widget.py")
}

pub(crate) fn generated_test_import_nameerror_projects_test_repair_fixture_passes() -> bool {
    let summary = r#"ERROR: test_calculator (unittest.loader._FailedTest.test_calculator)
----------------------------------------------------------------------
ImportError: Failed to import test module: test_calculator
Traceback (most recent call last):
  File "C:\Python313\Lib\unittest\loader.py", line 396, in _find_test_path
    module = self._get_module_from_name(name)
  File "C:\Python313\Lib\unittest\loader.py", line 339, in _get_module_from_name
    __import__(name)
    ~~~~~~~~~~^^^^^^
  File "C:\Users\example\Desktop\CodingAgent\project_sandbox\fr21-manual-st-loop-2026-05-27\required-core-rerun-003\case1\workspace\test_calculator.py", line 13, in <module>
    class TestAddition(FILE, API, BEH):
                       ^^^^
NameError: name 'FILE' is not defined

----------------------------------------------------------------------
Ran 1 test in 0.000s

FAILED (errors=1)"#;
    let evidence =
        verification_failure_evidence_from_summary(FailureKind::VerificationFailed, summary);
    let Some(item) = evidence.first() else {
        return false;
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-import-nameerror".to_string(),
        failing_labels: vec!["test_calculator".to_string()],
        primary_failure: Some("Command: python -m unittest".to_string()),
        evidence: evidence.clone(),
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };

    item.test_refs == vec!["test_calculator.py".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_artifact_name_resolution_defect")
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker.contains("generated test missing name `FILE`"))
        && projection.required_target.as_deref() == Some("test_calculator.py")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| template.source_test_ownership.contains("generated_test"))
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("test_calculator.py")
            })
}

pub(crate) fn generated_test_reflection_api_misuse_projects_test_repair_fixture_passes() -> bool {
    let summary = r#"ERROR: test_main_guard (test_calculator.TestCliEntrypointApi5Beh5.test_main_guard)
API-5: __name__ == '__main__' guard exists.
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_calculator.py", line 159, in test_main_guard
    source = inspect.getsource(main.__module__)
  File "C:\Python313\Lib\inspect.py", line 1258, in getsource
    lines, lnum = getsourcelines(object)
  File "C:\Python313\Lib\inspect.py", line 943, in getfile
    raise TypeError('module, class, method, function, traceback, frame, or code object was expected, got {}'.format(type(object).__name__))
TypeError: module, class, method, function, traceback, frame, or code object was expected, got str

----------------------------------------------------------------------
Ran 24 tests in 0.003s

FAILED (errors=1)"#;
    let evidence =
        verification_failure_evidence_from_summary(FailureKind::VerificationFailed, summary);
    let Some(item) = evidence.first() else {
        return false;
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-reflection-api-misuse".to_string(),
        failing_labels: vec!["test_main_guard".to_string()],
        primary_failure: Some("Command: python -m unittest".to_string()),
        evidence: evidence.clone(),
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.completion.verification_pending = true;
    state.verification.failure_cluster = Some(cluster);
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };

    item.test_refs == vec!["test_calculator.py".to_string()]
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker == "generated_test_artifact_api_misuse")
        && item
            .evidence_markers
            .iter()
            .any(|marker| marker.contains("generated test invalid reflection subject"))
        && projection.required_target.as_deref() == Some("test_calculator.py")
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
                    && snapshot.required_target.as_deref() == Some("test_calculator.py")
            })
}

pub(crate) fn repair_intent_defers_verification_command_evidence_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: test_widget import failed with SyntaxError".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec!["python -m unittest".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-deferred-verification-command-evidence".to_string(),
        failing_labels: vec!["test_widget".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("test_widget".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("SyntaxError: expected ':'".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `SyntaxError: expected ':'`".to_string(),
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
            !item.starts_with("Command:")
                && !item.contains("python -X utf8 -m unittest")
                && !item.contains("python -m unittest")
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
            .any(|command| command == "python -m unittest")
}

pub(crate) fn public_command_contract_failure_projects_compact_source_repair_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("tool.py"),
        Utf8PathBuf::from("test_tool.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "public_command_contract_failed: target=tool.py; observed=argv invocation entered interactive stdin mode instead of processing command-line arguments; expected=direct argv command handling preserves route-owned exit/stdout/stderr contract".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("tool.py")],
    });
    state.completion.verification_pending = true;
    state.verification.required_commands = vec![
        "python -X utf8 tool.py 2 + 3".to_string(),
        "python -X utf8 tool.py 8 +".to_string(),
    ];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-public-command-contract-failure".to_string(),
        failing_labels: vec!["failed command: python -X utf8 tool.py 2 + 3".to_string()],
        primary_failure: Some("public_command_contract_failed: target=tool.py; observed=argv invocation entered interactive stdin mode instead of processing command-line arguments; expected=direct argv command handling preserves route-owned exit/stdout/stderr contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_command_contract_failure".to_string()),
            label: Some("public_command_contract".to_string()),
            target: Some("tool.py".to_string()),
            symbol: None,
            call_site: Some("python -X utf8 tool.py 2 + 3".to_string()),
            exception: None,
            expected: Some("route-owned public argv command satisfies expected exit code and stdout/stderr observation".to_string()),
            observed: Some("argv invocation entered interactive stdin mode instead of processing command-line arguments".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_command_contract_failure".to_string(),
                "source_public_command_contract_assertion".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["tool.py".to_string()],
            test_refs: Vec::new(),
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["tool.py".to_string()],
        test_refs: Vec::new(),
    });

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
        && projection.required_target.as_deref() == Some("tool.py")
        && template.operation_kind == "source_public_command_contract"
        && template.source_test_ownership == "source"
        && intent.repair_owner == "source"
        && intent
            .required_edit_intent
            .contains("direct command-line invocations")
        && intent
            .required_evidence
            .iter()
            .any(|item| item.contains("public command contract failure"))
        && intent.required_evidence.iter().all(|item| {
            !item.contains("Traceback") && !item.contains("C:\\") && !item.contains("line 9")
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
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test requires an uncontracted logging side effect"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_visible_error_contract".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-contract-overreach".to_string(),
        failing_labels: vec!["test_visible_error_contract".to_string()],
        primary_failure: Some(
            "AssertionError: no logs of level ERROR or higher triggered".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generated_test_logging_contract_overreach".to_string()),
            label: Some("test_visible_error_contract".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: Some("with self.assertLogs(level=\"ERROR\") as captured".to_string()),
            exception: Some(
                "AssertionError: no logs of level ERROR or higher triggered".to_string(),
            ),
            expected: Some(
                "visible stderr/return-code contract without logging side effect".to_string(),
            ),
            observed: Some("generated test asserted logging side effect".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated-test logging side-effect assertion".to_string(),
                "generated_test_logging_contract_overreach".to_string(),
                "generated-test contract overreach".to_string(),
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
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

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

    projection.required_target.as_deref() == Some("test_widget.py")
        && template.exact_target.as_deref() == Some("test_widget.py")
        && template.operation_kind == "generated_test_logging_contract_repair"
        && template.source_test_ownership.contains("generated_test")
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "TestViolatesContract"
                    && decision.test_repair_allowed
                    && !decision.source_repair_allowed
                    && decision.required_target.as_deref() == Some("test_widget.py")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("test_widget.py")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn ungrounded_generated_public_output_assertion_projects_test_repair_fixture_passes()
-> bool {
    let summary = r#"FAIL: test_subprocess_eof_terminates (test_widget.TestCliSubprocess.test_subprocess_eof_terminates)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_widget.py", line 42, in test_subprocess_eof_terminates
    self.assertIn("goodbye", result.stdout)
AssertionError: 'goodbye' not found in 'Widget CLI ready\n>>> \n'

----------------------------------------------------------------------
Ran 35 tests in 0.288s

FAILED (failures=1)"#;
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test asserts uncontracted EOF stdout".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_subprocess_eof_terminates".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-ungrounded-generated-public-output".to_string(),
        failing_labels: vec!["test_subprocess_eof_terminates".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: public_output_stream_assertion_obligations(summary),
        source_refs: source_refs_from_summary(summary),
        test_refs: test_refs_from_summary(summary),
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("test_widget.py")
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
                    && decision.required_target.as_deref() == Some("test_widget.py")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("test_widget.py")
}

pub(crate) fn generated_test_public_output_numeric_format_overreach_projects_test_repair_fixture_passes()
-> bool {
    let summary = r#"FAIL: test_valid_division_expression (test_widget.TestCliSubprocess.test_valid_division_expression)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\project\test_widget.py", line 42, in test_valid_division_expression
    self.assertIn("Result: 5.0", proc.stdout)
    ~~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: 'Result: 5.0' not found in "Widget CLI\nInput: Result: 5\nInput: Bye\n"

----------------------------------------------------------------------
Ran 12 tests in 0.120s

FAILED (failures=1)"#;
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_valid_division_expression".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-public-output-numeric-format-overreach".to_string(),
        failing_labels: vec!["test_valid_division_expression".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: public_output_stream_assertion_obligations(summary),
        source_refs: source_refs_from_summary(summary),
        test_refs: test_refs_from_summary(summary),
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("test_widget.py")
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence.observed.as_deref().is_some_and(|observed| {
                        observed.contains("Result: 5") && !observed.contains("unmatched")
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
                    && decision.required_target.as_deref() == Some("test_widget.py")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("test_widget.py")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn generated_test_exception_type_overreach_projects_test_repair_fixture_passes() -> bool
{
    let summary = r#"ERROR: test_divide_by_zero (test_widget.TestWidget.test_divide_by_zero)
0 による除算で ZeroDivisionError が発生すること
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\project\test_widget.py", line 42, in test_divide_by_zero
    widget.divide(10, 0)
  File "C:\workspace\project\widget.py", line 7, in divide
    raise ValueError("division by zero")
ValueError: division by zero

----------------------------------------------------------------------
Ran 12 tests in 0.002s

FAILED (errors=1)"#;
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: summary.to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_divide_by_zero".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-exception-type-overreach".to_string(),
        failing_labels: vec!["test_divide_by_zero".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: Vec::new(),
        source_refs: source_refs_from_summary(summary),
        test_refs: test_refs_from_summary(summary),
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let allowed_tools = BTreeSet::from(["write".to_string(), "apply_patch".to_string()]);
    let Some(projection) = project_repair_lane(&state, &allowed_tools) else {
        return false;
    };
    let Some(snapshot) = projection.repair_control_snapshot.as_ref() else {
        return false;
    };

    projection.required_target.as_deref() == Some("test_widget.py")
        && projection
            .verification_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence.expected.as_deref() == Some("ZeroDivisionError")
                        && evidence.observed.as_deref() == Some("ValueError")
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
                    && decision.required_target.as_deref() == Some("test_widget.py")
            })
        && snapshot.repair_owner == "generated_test"
        && snapshot.required_target.as_deref() == Some("test_widget.py")
        && snapshot
            .hard_invariants
            .iter()
            .any(|invariant| invariant == "forbid_source_repair_for_generated_test_contract_owner")
}

pub(crate) fn generic_generated_test_only_repair_lane_preserves_active_test_target_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test stale literal".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_invalid_visible_contract".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generic-generated-test-only-repair-lane".to_string(),
        failing_labels: vec!["test_invalid_visible_contract".to_string()],
        primary_failure: Some("AssertionError: stale literal expectation".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_invalid_visible_contract".to_string()),
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
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());

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

    projection.required_target.as_deref() == Some("test_widget.py")
        && template.exact_target.as_deref() == Some("test_widget.py")
        && template.operation_kind == "source_test_contract_repair"
        && template.source_test_ownership == "source_or_generated_test_by_contract_evidence"
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceTestContractMismatch"
                    && decision.source_repair_allowed
                    && decision.test_repair_allowed
                    && decision.required_target.as_deref() == Some("test_widget.py")
            })
        && snapshot.required_target.as_deref() == Some("test_widget.py")
        && snapshot.repair_owner == "source_or_generated_test_by_contract_evidence"
}

pub(crate) fn contract_visible_public_exception_projects_source_repair_fixture_passes() -> bool {
    let summary = r#"FAIL: test_invalid_public_input (test_widget.TestWidget.test_invalid_public_input)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_widget.py", line 41, in test_invalid_public_input
    with self.assertRaises(ValueError):
AssertionError: ValueError not raised

----------------------------------------------------------------------
Ran 12 tests in 0.001s

FAILED (failures=1)"#;
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
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
    state.verification.failing_labels = vec!["test_invalid_public_input".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-contract-visible-public-exception-repair".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("AssertionError: ValueError not raised".to_string()),
        evidence: verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
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

    projection.required_target.as_deref() == Some("widget.py")
        && template.exact_target.as_deref() == Some("widget.py")
        && template.operation_kind == "source_exception_contract"
        && projection
            .contract_reconciliation
            .as_ref()
            .is_some_and(|decision| {
                decision.owner == "SourceViolatesContract"
                    && decision.source_repair_allowed
                    && !decision.test_repair_allowed
                    && decision.required_target.as_deref() == Some("widget.py")
            })
        && snapshot.repair_owner == "source"
        && snapshot.required_target.as_deref() == Some("widget.py")
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
struct GeneratedTestLoggingContractOverreach {
    logger_name: Option<String>,
    level: Option<String>,
    assertion_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicOutputStreamAssertionMismatch {
    stream: String,
    expected_substring: String,
    observed_value: String,
    observed_output: String,
    assertion_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicCommandContractFailure {
    command: Option<String>,
    observed_issue: String,
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

fn generated_test_logging_contract_overreach(
    summary: &str,
) -> Option<GeneratedTestLoggingContractOverreach> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("assertlogs(") || !lower.contains("no logs of level") {
        return None;
    }
    if !failure_summary_logical_lines(summary)
        .into_iter()
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("file ") && lower.contains(".py") && lower.contains("test_")
        })
    {
        return None;
    }
    let assertion_line = failure_summary_logical_lines(summary)
        .into_iter()
        .find(|line| line.to_ascii_lowercase().contains("assertlogs("))?
        .to_string();
    Some(GeneratedTestLoggingContractOverreach {
        logger_name: extract_assert_logs_logger(&assertion_line),
        level: extract_assert_logs_level(&assertion_line),
        assertion_line,
    })
}

fn public_output_stream_assertion_mismatch(
    summary: &str,
) -> Option<PublicOutputStreamAssertionMismatch> {
    let logical_lines = failure_summary_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(assert_start) = trimmed.find("self.assertIn(") {
            let Some(stream) = public_output_stream_subject(trimmed) else {
                continue;
            };
            let after = &trimmed[assert_start + "self.assertIn(".len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let args = top_level_arguments(after[..end].trim());
            let Some(expected) = args
                .first()
                .map(|value| clean_output_assertion_value(value))
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let observed = subsequent_assertion_not_found_observed_value(&logical_lines, index)
                .unwrap_or_else(|| format!("unmatched {stream} output"));
            let observed_output = if observed.is_empty() {
                format!("empty {stream}")
            } else {
                format!("{stream} `{observed}`")
            };
            return Some(PublicOutputStreamAssertionMismatch {
                stream: stream.to_string(),
                expected_substring: expected,
                observed_value: observed,
                observed_output,
                assertion_line: trimmed.to_string(),
            });
        }
        if let Some(assert_start) = trimmed.find("self.assertEqual(") {
            let after = &trimmed[assert_start + "self.assertEqual(".len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let args = top_level_arguments(after[..end].trim());
            if args.len() < 2 {
                continue;
            }
            let Some((stream, expected)) = public_output_assert_equal_stream_and_expected(&args)
            else {
                continue;
            };
            let (observed, error_expected) =
                subsequent_assertion_equal_observed_expected_values(&logical_lines, index)
                    .unwrap_or_else(|| (format!("unmatched {stream} output"), expected.clone()));
            let expected = if !error_expected.is_empty() {
                error_expected
            } else {
                expected
            };
            let observed_output = if observed.is_empty() {
                format!("empty {stream}")
            } else {
                format!("{stream} `{observed}`")
            };
            return Some(PublicOutputStreamAssertionMismatch {
                stream: stream.to_string(),
                expected_substring: expected,
                observed_value: observed,
                observed_output,
                assertion_line: trimmed.to_string(),
            });
        }
    }
    None
}

fn generated_test_subprocess_output_capture_missing(
    summary: &str,
) -> Option<PublicOutputStreamAssertionMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("typeerror:")
        && lower.contains("nonetype")
        && lower.contains("not iterable"))
    {
        return None;
    }
    if test_refs_from_summary(summary).is_empty() {
        return None;
    }
    for line in failure_summary_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(assert_start) = trimmed.find("self.assertIn(") else {
            continue;
        };
        let Some(stream) = public_output_stream_subject(trimmed) else {
            continue;
        };
        let after = &trimmed[assert_start + "self.assertIn(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(after[..end].trim());
        let Some(expected) = args
            .first()
            .map(|value| clean_output_assertion_value(value))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return Some(PublicOutputStreamAssertionMismatch {
            stream: stream.to_string(),
            expected_substring: expected,
            observed_value: format!("CompletedProcess.{stream} is None"),
            observed_output: format!(
                "CompletedProcess.{stream} is None because generated subprocess.run did not capture {stream}"
            ),
            assertion_line: trimmed.to_string(),
        });
    }
    None
}

fn generated_test_subprocess_encoding_missing(
    summary: &str,
) -> Option<PublicOutputStreamAssertionMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("unicodedecodeerror")
        && lower.contains("utf-8")
        && lower.contains("subprocess.py")
        && lower.contains("_readerthread")
        && lower.contains("nonetype")
        && lower.contains("not iterable"))
    {
        return None;
    }
    if test_refs_from_summary(summary).is_empty() {
        return None;
    }
    for line in failure_summary_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(assert_start) = trimmed.find("self.assertIn(") else {
            continue;
        };
        let Some(stream) = public_output_stream_subject(trimmed) else {
            continue;
        };
        let after = &trimmed[assert_start + "self.assertIn(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(after[..end].trim());
        let Some(expected) = args
            .first()
            .map(|value| clean_output_assertion_value(value))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return Some(PublicOutputStreamAssertionMismatch {
            stream: stream.to_string(),
            expected_substring: expected,
            observed_value: format!("CompletedProcess.{stream} is None after UnicodeDecodeError"),
            observed_output: format!(
                "UnicodeDecodeError while parent decoded child subprocess {stream} as UTF-8 without explicit child UTF-8 output authority"
            ),
            assertion_line: trimmed.to_string(),
        });
    }
    None
}

fn public_output_assert_equal_stream_and_expected(args: &[&str]) -> Option<(&'static str, String)> {
    let first_stream = args
        .first()
        .and_then(|arg| public_output_stream_subject(arg));
    let second_stream = args
        .get(1)
        .and_then(|arg| public_output_stream_subject(arg));
    if let Some(stream) = first_stream {
        return args
            .get(1)
            .map(|arg| clean_output_assertion_value(arg))
            .filter(|value| !value.is_empty())
            .map(|expected| (stream, expected));
    }
    if let Some(stream) = second_stream {
        return args
            .first()
            .map(|arg| clean_output_assertion_value(arg))
            .filter(|value| !value.is_empty())
            .map(|expected| (stream, expected));
    }
    None
}

fn public_output_stream_subject(assertion_line: &str) -> Option<&'static str> {
    if assertion_line.contains("result.stderr") || assertion_line.contains(".stderr") {
        Some("stderr")
    } else if assertion_line.contains("result.stdout") || assertion_line.contains(".stdout") {
        Some("stdout")
    } else {
        None
    }
}

fn assertion_not_found_observed_value(line: &str) -> Option<String> {
    let detail = line.trim().strip_prefix("AssertionError:")?.trim();
    let (_, observed) = detail.split_once(" not found in ")?;
    Some(clean_output_assertion_value(observed))
}

fn subsequent_assertion_not_found_observed_value(lines: &[&str], index: usize) -> Option<String> {
    lines
        .iter()
        .skip(index + 1)
        .take(6)
        .find_map(|line| assertion_not_found_observed_value(line))
}

fn assertion_equal_observed_expected_values(line: &str) -> Option<(String, String)> {
    let detail = line.trim().strip_prefix("AssertionError:")?.trim();
    let (observed, expected) = detail.split_once("!=")?;
    Some((
        clean_output_assertion_value(observed),
        clean_output_assertion_value(expected),
    ))
}

fn subsequent_assertion_equal_observed_expected_values(
    lines: &[&str],
    index: usize,
) -> Option<(String, String)> {
    lines
        .iter()
        .skip(index + 1)
        .take(8)
        .find_map(|line| assertion_equal_observed_expected_values(line))
}

fn clean_output_assertion_value(value: &str) -> String {
    let value = value.trim().trim_end_matches(',').trim();
    if value.len() >= 2 {
        let mut chars = value.chars();
        let first = chars.next();
        let last = value.chars().last();
        if matches!(
            (first, last),
            (Some('\''), Some('\'')) | (Some('"'), Some('"'))
        ) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn extract_assert_logs_logger(assertion_line: &str) -> Option<String> {
    extract_delimited_after(assertion_line, "assertLogs(\"", '"')
        .or_else(|| extract_delimited_after(assertion_line, "assertLogs('", '\''))
}

fn extract_assert_logs_level(assertion_line: &str) -> Option<String> {
    extract_delimited_after(assertion_line, "level=\"", '"')
        .or_else(|| extract_delimited_after(assertion_line, "level='", '\''))
}

fn extract_delimited_after(text: &str, marker: &str, terminator: char) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find(terminator)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn required_target_for_subtype(
    state: &SessionStateSnapshot,
    subtype: &RepairLaneSubtype,
    cluster: Option<&VerificationFailureCluster>,
) -> Option<String> {
    match subtype {
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
        | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
        | RepairLaneSubtype::GeneratedTestLoggingContractOverreach => {
            first_test_target(&state.active_targets).or_else(|| first_target(&state.active_targets))
        }
        RepairLaneSubtype::ImportExportMissingExport => import_export_source_target(state, cluster)
            .or_else(|| first_non_test_target(&state.active_targets))
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
            .or_else(|| first_non_test_failure_target(state))
            .or_else(|| first_source_ref_target(cluster))
            .or_else(|| source_target_from_cluster_test_refs(cluster)),
        RepairLaneSubtype::PublicCallableSignatureMismatch => cluster
            .and_then(|cluster| {
                cluster
                    .evidence
                    .iter()
                    .filter_map(|evidence| evidence.target.clone())
                    .find(|target| target_is_mutable_source_like(target))
            })
            .or_else(|| first_non_test_target(&state.active_targets)),
        RepairLaneSubtype::PublicMethodAttributeMismatch => {
            first_non_test_target(&state.active_targets)
                .or_else(|| first_non_test_failure_target(state))
                .or_else(|| first_source_ref_target(cluster))
        }
        RepairLaneSubtype::PublicExceptionMismatch => cluster
            .and_then(|cluster| {
                cluster
                    .evidence
                    .iter()
                    .filter_map(|evidence| evidence.target.clone())
                    .find(|target| target_is_mutable_source_like(target))
            })
            .or_else(|| first_non_test_target(&state.active_targets)),
        RepairLaneSubtype::PatchMismatch | RepairLaneSubtype::GenericVerificationFailure => {
            first_target(&state.active_targets)
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

fn typed_repair_target_outranks_required_action(
    subtype: &RepairLaneSubtype,
    typed_target: Option<&str>,
    action_target: Option<&str>,
    cluster: Option<&VerificationFailureCluster>,
) -> bool {
    let (Some(typed_target), Some(action_target)) = (typed_target, action_target) else {
        return false;
    };
    if typed_target == action_target
        || target_is_test_like(typed_target)
        || !target_is_test_like(action_target)
    {
        return false;
    }
    match subtype {
        RepairLaneSubtype::PublicMissingAttributeMismatch
        | RepairLaneSubtype::PublicCommandContractFailure
        | RepairLaneSubtype::PublicOutputStreamAssertionMismatch
        | RepairLaneSubtype::PublicStateAssertionMismatch => {
            !has_explicit_generated_test_conflict_evidence_in_cluster(cluster)
        }
        RepairLaneSubtype::PublicClassAttributeMismatch
        | RepairLaneSubtype::PublicConstructorBodyException
        | RepairLaneSubtype::PublicConstructorSignatureMismatch
        | RepairLaneSubtype::PublicCallableSignatureMismatch
        | RepairLaneSubtype::PublicExceptionMismatch
        | RepairLaneSubtype::PublicMethodAttributeMismatch
        | RepairLaneSubtype::ImportExportMissingExport
        | RepairLaneSubtype::SourceImportTimeNameResolution
        | RepairLaneSubtype::SourceParseDefect => true,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
        | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
        | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
        | RepairLaneSubtype::NoTestsRan
        | RepairLaneSubtype::PatchMismatch
        | RepairLaneSubtype::GenericVerificationFailure => false,
    }
}

fn no_tests_ran_generated_test_target_outranks_stale_write_action(
    subtype: &RepairLaneSubtype,
    typed_target: Option<&str>,
) -> bool {
    let _ = (subtype, typed_target);
    false
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

fn first_non_test_failure_target(state: &SessionStateSnapshot) -> Option<String> {
    state
        .failure
        .as_ref()
        .and_then(|failure| first_non_test_target(&failure.targets))
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
        .is_some_and(target_is_mutable_source_like)
    {
        if let Some(target) = required_target.as_deref()
            && let Some(canonical) = canonical_relative_source_target_for(target, state, cluster)
        {
            return Some(canonical);
        }
        return required_target;
    }

    first_non_test_target(&state.active_targets)
        .or_else(|| first_non_test_failure_target(state))
        .or_else(|| first_source_ref_target(cluster))
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
    let candidate = candidate.replace('\\', "/");
    let target = target.replace('\\', "/");
    candidate.eq_ignore_ascii_case(&target)
        || target
            .to_ascii_lowercase()
            .ends_with(&format!("/{}", candidate.to_ascii_lowercase()))
        || candidate
            .to_ascii_lowercase()
            .ends_with(&format!("/{}", target.to_ascii_lowercase()))
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
    python_source_for_test_target(target)
        .map(|contract| contract.source_path)
        .filter(|source| target_is_mutable_source_like(source))
}

fn target_is_test_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".spec.js")
        || normalized.contains("/tests/")
}

fn target_is_mutable_source_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    !target_is_test_like(target)
        && !matches!(file_name, "scenario_contract.md" | "scenario_contract.json")
        && (normalized.contains("/src/")
            || file_name.ends_with(".py")
            || file_name.ends_with(".rs")
            || file_name.ends_with(".js")
            || file_name.ends_with(".ts")
            || file_name.ends_with(".tsx")
            || file_name.ends_with(".jsx"))
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
        test_refs: test_refs_from_summary(summary),
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
        | RepairLaneSubtype::NoTestsRan => test_refs_from_summary(summary).into_iter().next(),
        RepairLaneSubtype::PatchMismatch | RepairLaneSubtype::GenericVerificationFailure => None,
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
        _ => None,
    }
}

fn source_refs_from_summary(summary: &str) -> Vec<String> {
    file_refs_from_summary(summary, false)
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
    file_refs_from_summary(summary, true)
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
    if generated_test_public_output_contract_overreach(summary).is_some() {
        markers.push("generated_test_contract_overreach".to_string());
    }
    if generated_test_exception_type_overreach(summary).is_some() {
        markers.push("generated_test_contract_overreach".to_string());
    }
    markers
}

fn file_refs_from_summary(summary: &str, tests: bool) -> Vec<String> {
    let mut refs = failure_summary_logical_lines(summary)
        .into_iter()
        .filter_map(|line| quoted_file_frame_path(line))
        .filter(|path| !runtime_traceback_frame_path(path))
        .filter(|path| target_is_test_like(path) == tests)
        .map(|path| {
            path.replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(path.as_str())
                .to_string()
        })
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    refs
}

fn runtime_traceback_frame_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/lib/unittest/")
        || normalized.contains("/lib/site-packages/")
        || normalized.contains("/lib/python")
        || normalized.contains("/python")
            && normalized.contains("/lib/")
            && !normalized.contains("/workspace/")
            && !normalized.contains("/project_sandbox/")
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

fn public_state_assertions(summary: &str) -> Vec<String> {
    let mut assertions = public_state_assertions_from_normalized_feedback(summary);
    assertions.extend(public_collection_access_failures(summary));
    let logical_lines = failure_summary_logical_lines(summary);
    for (line_index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        for marker in [
            "self.assertTrue(",
            "self.assertFalse(",
            "self.assertEqual(",
            "self.assertNotEqual(",
            "self.assertAlmostEqual(",
            "self.assertLess(",
            "self.assertLessEqual(",
            "self.assertGreater(",
            "self.assertGreaterEqual(",
        ] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let inside = after[..end].trim();
            let subject = first_call_argument(inside).unwrap_or(inside).trim();
            if subject.is_empty() {
                continue;
            }
            let subject = enriched_assertion_subject(&logical_lines[..line_index], subject);
            if !assertions
                .iter()
                .any(|existing: &String| existing == &subject)
            {
                assertions.push(subject);
            }
        }
    }
    assertions
}

pub(crate) fn public_state_assertion_observations(summary: &str) -> Vec<String> {
    let mut observations = public_state_observations_from_normalized_feedback(summary);
    observations.extend(public_collection_access_observations(summary));
    let logical_lines = failure_summary_logical_lines(summary);
    for (line_index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        for marker in [
            "self.assertTrue(",
            "self.assertFalse(",
            "self.assertEqual(",
            "self.assertNotEqual(",
            "self.assertAlmostEqual(",
            "self.assertLess(",
            "self.assertLessEqual(",
            "self.assertGreater(",
            "self.assertGreaterEqual(",
        ] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let inside = after[..end].trim();
            let args = top_level_arguments(inside);
            let Some(subject) = args
                .first()
                .map(|arg| arg.trim())
                .filter(|arg| !arg.is_empty())
            else {
                continue;
            };
            let subject = enriched_assertion_subject(&logical_lines[..line_index], subject);
            let expected = expected_value_for_assertion(marker, &args);
            let actual = assertion_error_actual_value(logical_lines.get(line_index + 1).copied());
            let observation = match (expected, actual) {
                (Some(expected), Some(actual)) => {
                    format!("`{subject}` expected `{expected}` but observed `{actual}`")
                }
                (Some(expected), None) => format!("`{subject}` expected `{expected}`"),
                (None, Some(actual)) => format!("`{subject}` observed `{actual}`"),
                (None, None) => format!("`{subject}`"),
            };
            if !observations
                .iter()
                .any(|existing: &String| existing == &observation)
            {
                observations.push(observation);
            }
        }
    }
    observations
}

pub(crate) fn public_state_terminal_transition_obligations(summary: &str) -> Vec<String> {
    let logical_lines = failure_summary_logical_lines(summary);
    let mut obligations = Vec::new();
    for line in logical_lines {
        let trimmed = line.trim();
        let Some(start) = trimmed.find("self.assertEqual(") else {
            continue;
        };
        let after = &trimmed[start + "self.assertEqual(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(after[..end].trim());
        let Some(subject) = args.first().map(|arg| arg.trim()) else {
            continue;
        };
        let Some(expected) = args.get(1).map(|arg| arg.trim()) else {
            continue;
        };
        if !is_public_state_subject(subject) || !is_terminal_state_expected(expected) {
            continue;
        }
        let obligation = format!("{subject} terminal transition to {expected}");
        if !obligations
            .iter()
            .any(|existing: &String| existing == &obligation)
        {
            obligations.push(obligation);
        }
    }
    obligations
}

fn is_public_state_subject(subject: &str) -> bool {
    let normalized = subject.trim().trim_matches('`');
    normalized == "state"
        || normalized.ends_with(".state")
        || normalized.contains(".state.")
        || normalized.ends_with("_state")
        || normalized.ends_with(".status")
        || normalized.ends_with("_status")
}

fn is_terminal_state_expected(expected: &str) -> bool {
    let normalized = expected
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_uppercase();
    normalized.contains("GAME_OVER")
        || normalized.contains(".WIN")
        || normalized.contains(".WON")
        || normalized.ends_with("WIN")
        || normalized.ends_with("WON")
        || normalized.contains("COMPLETE")
        || normalized.contains("COMPLETED")
        || normalized.contains("FINISH")
        || normalized.contains("ENDED")
        || normalized.contains("FAIL")
        || normalized.contains("SUCCESS")
}

fn public_state_assertions_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) =
        summary.split_once("Public state assertion mismatch detected for ")
    else {
        return Vec::new();
    };
    let end = after_marker
        .find(": expected public state")
        .or_else(|| after_marker.find(". Observed mismatch"))
        .unwrap_or(after_marker.len());
    backtick_values(&after_marker[..end])
}

fn public_state_observations_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) = summary.split_once("Observed mismatch:") else {
        return Vec::new();
    };
    let end = after_marker
        .find(". For ")
        .or_else(|| after_marker.find(". Latest "))
        .or_else(|| after_marker.find(". Do not "))
        .unwrap_or(after_marker.len());
    let mut observations = Vec::new();
    for clause in after_marker[..end].split(';') {
        let values = backtick_values(clause);
        if values.len() >= 3 {
            observations.push(format!(
                "`{}` expected `{}` but observed `{}`",
                values[0], values[1], values[2]
            ));
        }
    }
    observations
}

fn public_collection_access_failures(summary: &str) -> Vec<String> {
    let logical_lines = failure_summary_logical_lines(summary);
    let mut accesses = Vec::new();
    for (line_index, line) in logical_lines.iter().enumerate() {
        if !line.contains("IndexError: list index out of range") {
            continue;
        }
        let Some(access) = preceding_collection_access(&logical_lines[..line_index]) else {
            continue;
        };
        if !accesses.iter().any(|existing| existing == &access) {
            accesses.push(access);
        }
    }
    accesses
}

fn public_collection_access_observations(summary: &str) -> Vec<String> {
    public_collection_access_failures(summary)
        .into_iter()
        .map(|access| format!("`{access}` expected collection element but observed `IndexError`"))
        .collect()
}

fn preceding_collection_access(previous_lines: &[&str]) -> Option<String> {
    previous_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || !trimmed.contains('[')
            || !trimmed.contains(']')
        {
            return None;
        }
        first_collection_access(trimmed)
    })
}

fn first_collection_access(line: &str) -> Option<String> {
    let open = line.find('[')?;
    let close = line[open..].find(']')? + open;
    let mut start = open;
    while start > 0 {
        let ch = line.as_bytes()[start - 1] as char;
        if ch == '_' || ch == '.' || ch.is_ascii_alphanumeric() {
            start -= 1;
        } else {
            break;
        }
    }
    if start == open {
        return None;
    }
    Some(line[start..=close].trim().to_string())
}

fn backtick_values(text: &str) -> Vec<String> {
    text.split('`')
        .enumerate()
        .filter_map(|(index, value)| {
            (index % 2 == 1 && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
        .collect()
}

pub(crate) fn public_state_game_loop_operation_obligations(
    summary: &str,
    assertions: &[String],
) -> Vec<String> {
    let lower = summary.to_ascii_lowercase();
    let assertion_lower = assertions
        .iter()
        .map(|assertion| assertion.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut obligations = Vec::new();

    let has_projectile = lower.contains("projectile")
        || lower.contains("bullet")
        || assertion_lower
            .iter()
            .any(|assertion| assertion.contains("projectile") || assertion.contains("bullet"));
    let has_projectile_y = assertion_lower
        .iter()
        .any(|assertion| assertion.contains(".y") && has_projectile);
    if has_projectile_y
        && (lower.contains("move")
            || lower.contains("tick")
            || lower.contains("direction")
            || lower.contains("expected `110`")
            || lower.contains("expected `490`"))
    {
        obligations.push("projectile movement delta".to_string());
    }

    let has_projectile_active = assertion_lower
        .iter()
        .any(|assertion| assertion.contains(".active") && has_projectile);
    if has_projectile_active
        && (lower.contains("out_of_bounds")
            || lower.contains("out of bounds")
            || lower.contains("offscreen")
            || lower.contains("bounds")
            || lower.contains("expected `false`")
            || lower.contains("true is not false"))
    {
        obligations.push("projectile bounds lifecycle".to_string());
    }

    let has_spawn_coordinate = assertion_lower
        .iter()
        .any(|assertion| assertion.contains(".x") && has_projectile);
    let has_spawn_count = assertion_lower.iter().any(|assertion| {
        assertion.starts_with("len(")
            && (assertion.contains("projectile")
                || assertion.contains("bullet")
                || assertion.contains("shots"))
    });
    if (has_spawn_coordinate || has_spawn_count)
        && (lower.contains("spawn")
            || lower.contains("shoot")
            || lower.contains("fire")
            || lower.contains("create"))
    {
        obligations.push("projectile spawn coordinate and repeated spawn allowance".to_string());
    }

    let has_life_or_counter = assertion_lower.iter().any(|assertion| {
        assertion.contains(".lives")
            || assertion.contains(".life")
            || assertion.contains(".health")
            || assertion.contains(".score")
    });
    if has_projectile
        && has_life_or_counter
        && (lower.contains("hit") || lower.contains("collision") || lower.contains("collides"))
    {
        obligations.push("projectile collision counter/lifecycle update".to_string());
    }

    if !public_state_terminal_transition_obligations(summary).is_empty()
        || (lower.contains("reaches")
            && (lower.contains("bottom")
                || lower.contains("boundary")
                || lower.contains("terminal")))
    {
        obligations.push("terminal boundary predicate".to_string());
    }

    let has_moved_marker = assertion_lower
        .iter()
        .any(|assertion| assertion == "moved" || assertion.ends_with(".moved"));
    if has_moved_marker
        || ((lower.contains("tick") || lower.contains("update"))
            && (lower.contains("moves_entities")
                || lower.contains("moves_invaders")
                || lower.contains("entity_move")
                || lower.contains("group")
                || lower.contains("moved)")))
    {
        obligations.push("entity group movement update".to_string());
    }

    obligations.sort();
    obligations.dedup();
    obligations
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

fn has_explicit_generated_test_conflict_evidence_in_cluster(
    cluster: Option<&VerificationFailureCluster>,
) -> bool {
    cluster_evidence_markers(cluster)
        .iter()
        .any(|marker| has_explicit_generated_test_conflict_evidence(marker))
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

pub(crate) fn public_api_data_model_semantic_obligations(summary: &str) -> Vec<String> {
    let lower = summary.to_ascii_lowercase();
    let mut obligations = Vec::new();

    if let Some(mismatch) = public_constructor_signature_mismatch(summary) {
        let keywords = mismatch
            .call_site
            .as_deref()
            .map(call_site_keyword_arguments)
            .unwrap_or_default();
        if keywords.is_empty() {
            obligations.push(format!(
                "constructor keyword compatibility for `{}`",
                mismatch.constructor
            ));
        } else {
            obligations.push(format!(
                "constructor keyword compatibility for `{}` fields ({})",
                mismatch.constructor,
                keywords
                    .iter()
                    .map(|keyword| format!("`{keyword}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if has_enum_primitive_value_assertion(summary) {
        obligations.push("enum primitive value representation".to_string());
    }
    if lower.contains("move")
        && lower.contains("assertionerror:")
        && (lower.contains(".x") || lower.contains(".y") || lower.contains("boundary"))
    {
        obligations.push("no-argument public movement default and boundary semantics".to_string());
    }
    if (lower.contains("initial_positions")
        || lower.contains("assertnotequal")
        || lower.contains("not equal"))
        && (lower.contains("move") || lower.contains("update"))
    {
        obligations.push("direct public movement/update mutates caller-visible state".to_string());
    }

    obligations.sort();
    obligations.dedup();
    obligations
}

fn call_site_keyword_arguments(call_site: &str) -> Vec<String> {
    let Some(arguments) = call_site
        .split_once('(')
        .and_then(|(_, tail)| tail.rsplit_once(')').map(|(inside, _)| inside))
    else {
        return Vec::new();
    };
    let mut keywords = top_level_arguments(arguments)
        .into_iter()
        .filter_map(|argument| argument.split_once('=').map(|(keyword, _)| keyword.trim()))
        .filter(|keyword| {
            !keyword.is_empty()
                && keyword
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    keywords.sort();
    keywords.dedup();
    keywords
}

fn has_enum_primitive_value_assertion(summary: &str) -> bool {
    failure_summary_logical_lines(summary)
        .into_iter()
        .any(|line| {
            let Some(detail) = line.trim().strip_prefix("AssertionError:") else {
                return false;
            };
            detail.contains('<')
                && detail.contains(':')
                && detail.contains('>')
                && (detail.contains(" != '")
                    || detail.contains(" != \"")
                    || detail.contains(" != 0")
                    || detail.contains(" != 1"))
        })
}

fn public_method_sibling_obligations(summary: &str) -> Vec<String> {
    let attrs = public_missing_attributes(summary);
    let mut obligations = attrs
        .iter()
        .filter(|attribute| {
            let receiver = attribute.split('.').next().unwrap_or_default();
            matches!(receiver, "int" | "str" | "float" | "bool" | "list" | "dict")
        })
        .map(|attribute| format!("collection element shape defect `{attribute}`"))
        .collect::<Vec<_>>();
    obligations.sort();
    obligations.dedup();
    obligations
}

fn failure_summary_logical_lines(summary: &str) -> Vec<&str> {
    summary
        .lines()
        .flat_map(|line| line.split('|'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn first_call_argument(arguments: &str) -> Option<&str> {
    top_level_arguments(arguments).into_iter().next()
}

fn top_level_arguments(arguments: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in arguments.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(arguments[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = arguments[start..].trim();
    if !tail.is_empty() {
        args.push(tail);
    }
    args
}

fn expected_value_for_assertion(marker: &str, args: &[&str]) -> Option<String> {
    if marker.contains("assertTrue") {
        return Some("truthy".to_string());
    }
    if marker.contains("assertFalse") {
        return Some("false".to_string());
    }
    if marker.contains("assertLessEqual") {
        return args
            .get(1)
            .map(|value| format!("<= {}", clean_assertion_value(value)));
    }
    if marker.contains("assertLess") {
        return args
            .get(1)
            .map(|value| format!("< {}", clean_assertion_value(value)));
    }
    if marker.contains("assertGreaterEqual") {
        return args
            .get(1)
            .map(|value| format!(">= {}", clean_assertion_value(value)));
    }
    if marker.contains("assertGreater") {
        return args
            .get(1)
            .map(|value| format!("> {}", clean_assertion_value(value)));
    }
    args.get(1)
        .map(|value| clean_assertion_value(value))
        .filter(|value| !value.is_empty())
}

fn assertion_error_actual_value(line: Option<&str>) -> Option<String> {
    let line = line?.trim();
    let detail = line.strip_prefix("AssertionError:")?.trim();
    if let Some((actual, _)) = detail.split_once("!=") {
        return Some(clean_assertion_value(actual));
    }
    if detail.contains("False is not true") {
        return Some("False".to_string());
    }
    if detail.contains("True is not false") {
        return Some("True".to_string());
    }
    for marker in [
        " not less than or equal to ",
        " not greater than or equal to ",
        " not less than ",
        " not greater than ",
    ] {
        if let Some((actual, _expected)) = detail.split_once(marker) {
            return Some(clean_assertion_value(actual));
        }
    }
    None
}

fn clean_assertion_value(value: &str) -> String {
    value
        .split(" within ")
        .next()
        .unwrap_or(value)
        .trim()
        .trim_end_matches(',')
        .trim()
        .to_string()
}

fn enriched_assertion_subject(previous_lines: &[&str], subject: &str) -> String {
    let Some(root) = root_identifier(subject) else {
        return subject.to_string();
    };
    let Some(rhs) = previous_assignment_rhs(previous_lines, root) else {
        return subject.to_string();
    };
    if subject == root {
        format!("{root} = {rhs}")
    } else {
        format!("{subject} from {root} = {rhs}")
    }
}

fn root_identifier(subject: &str) -> Option<&str> {
    let subject = subject.trim();
    let mut end = 0usize;
    for (index, ch) in subject.char_indices() {
        if index == 0 {
            if !(ch == '_' || ch.is_ascii_alphabetic()) {
                return None;
            }
            end = ch.len_utf8();
            continue;
        }
        if ch == '_' || ch.is_ascii_alphanumeric() {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| &subject[..end])
}

fn previous_assignment_rhs<'a>(previous_lines: &'a [&'a str], variable: &str) -> Option<&'a str> {
    previous_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix(variable)?.trim_start();
        let rhs = rest.strip_prefix('=')?.trim();
        (!rhs.is_empty()).then_some(rhs)
    })
}

fn public_missing_attributes(summary: &str) -> Vec<String> {
    let mut attributes = public_missing_attributes_from_normalized_feedback(summary);
    attributes.extend(public_writable_property_obligations(summary));
    for line in failure_summary_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        if !detail.contains(" has no attribute ") {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let attr = format!("{}.{}", quoted[0].trim(), quoted[1].trim());
        if !attributes.iter().any(|existing| existing == &attr) {
            attributes.push(attr);
        }
    }
    attributes
}

fn public_writable_property_obligations(summary: &str) -> Vec<String> {
    let mut obligations = Vec::new();
    for line in failure_summary_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        let detail = detail.trim();
        if !detail.contains("property ")
            || !detail.contains(" object has no setter")
            || !detail.contains(" of ")
        {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let property = quoted[0].trim();
        let owner = quoted[1].trim();
        if property.is_empty() || owner.is_empty() {
            continue;
        }
        let obligation = format!("{owner}.{property} writable property");
        if !obligations
            .iter()
            .any(|existing: &String| existing == &obligation)
        {
            obligations.push(obligation);
        }
    }
    obligations
}

fn public_missing_attributes_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) =
        summary.split_once("Public missing-attribute mismatch detected for ")
    else {
        return Vec::new();
    };
    let end = after_marker
        .find(". Align ")
        .or_else(|| after_marker.find(". Latest "))
        .or_else(|| after_marker.find(". Required "))
        .unwrap_or(after_marker.len());
    backtick_values(&after_marker[..end])
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicMissingMethodAttribute {
    attribute: String,
    call_site: String,
}

fn public_missing_method_attributes(summary: &str) -> Vec<PublicMissingMethodAttribute> {
    let logical_lines = failure_summary_logical_lines(summary);
    let mut methods = Vec::new();
    for (line_index, line) in logical_lines.iter().enumerate() {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        if !detail.contains(" has no attribute ") {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let receiver = quoted[0].trim();
        let member = quoted[1].trim();
        let Some(call_site) = missing_method_call_site_before(&logical_lines[..line_index], member)
        else {
            continue;
        };
        let attribute = format!("{receiver}.{member}");
        if !methods
            .iter()
            .any(|existing: &PublicMissingMethodAttribute| existing.attribute == attribute)
        {
            methods.push(PublicMissingMethodAttribute {
                attribute,
                call_site,
            });
        }
    }
    methods
}

fn missing_method_call_site_before(lines: &[&str], member: &str) -> Option<String> {
    let needle = format!(".{member}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("attributeerror:")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || !trimmed.contains(&needle)
        {
            return None;
        }
        Some(trimmed.to_string())
    })
}

fn public_class_or_enum_missing_members(summary: &str) -> Vec<String> {
    let mut members = Vec::new();
    for detail in public_class_or_enum_missing_member_details(summary) {
        let member = detail.member;
        if !members.iter().any(|existing| existing == &member) {
            members.push(member);
        }
    }
    members
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicClassOrEnumMissingMemberDetail {
    member: String,
    suggested_existing_member: Option<String>,
    expected_value: Option<String>,
}

fn public_class_or_enum_missing_member_details(
    summary: &str,
) -> Vec<PublicClassOrEnumMissingMemberDetail> {
    let mut details = Vec::new();
    for line in failure_summary_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        let detail = detail.trim();
        if !(detail.starts_with("type object ") || detail.starts_with("module "))
            || !detail.contains(" has no attribute ")
        {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let owner = quoted[0].trim();
        let missing = quoted[1].trim();
        let member = format!("{owner}.{missing}");
        if details
            .iter()
            .any(|existing: &PublicClassOrEnumMissingMemberDetail| existing.member == member)
        {
            continue;
        }
        let suggested_existing_member =
            extract_quoted_after(detail, "Did you mean: '").map(|suggested| {
                if suggested.contains('.') {
                    suggested
                } else {
                    format!("{owner}.{suggested}")
                }
            });
        let expected_value = expected_value_for_class_member(summary, &member);
        details.push(PublicClassOrEnumMissingMemberDetail {
            member,
            suggested_existing_member,
            expected_value,
        });
    }
    details
}

pub(crate) fn public_class_member_repair_observations(summary: &str) -> Vec<String> {
    public_class_or_enum_missing_member_details(summary)
        .into_iter()
        .map(|detail| {
            let mut observation = format!("`{}` is missing", detail.member);
            if let Some(suggested) = detail.suggested_existing_member {
                observation.push_str(&format!("; source near-name candidate is `{suggested}`"));
            }
            if let Some(expected) = detail.expected_value {
                observation.push_str(&format!(
                    "; generated-test value contract expects `{}.value == {expected}`",
                    detail.member
                ));
            }
            observation
        })
        .collect()
}

fn expected_value_for_class_member(summary: &str, member: &str) -> Option<String> {
    let value_ref = format!("{member}.value");
    for line in failure_summary_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(start) = trimmed.find("self.assertEqual(") else {
            continue;
        };
        let after = &trimmed[start + "self.assertEqual(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(&after[..end]);
        if args.first().map(|arg| arg.trim()) != Some(value_ref.as_str()) {
            continue;
        }
        return args
            .get(1)
            .map(|value| clean_assertion_value(value))
            .filter(|value| !value.is_empty());
    }
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicConstructorSignatureMismatch {
    constructor: String,
    detail: String,
    unexpected_keyword: Option<String>,
    call_site: Option<String>,
}

fn public_constructor_signature_mismatch(
    summary: &str,
) -> Option<PublicConstructorSignatureMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || !lower.contains("__init__()")
        || !(lower.contains("unexpected keyword argument")
            || lower.contains("positional argument")
            || lower.contains("takes "))
    {
        return None;
    }

    let logical_lines = failure_summary_logical_lines(summary);
    let detail_index = logical_lines.iter().position(|line| {
        let lower_line = line.to_ascii_lowercase();
        lower_line.contains("typeerror:") && lower_line.contains("__init__()")
    })?;
    let detail = logical_lines[detail_index]
        .split("TypeError:")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let constructor = detail
        .split(".__init__()")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let unexpected_keyword = extract_quoted_after(&detail, "unexpected keyword argument '");
    let call_site =
        constructor_call_site_before(&logical_lines[..detail_index], constructor.as_str());

    Some(PublicConstructorSignatureMismatch {
        constructor,
        detail,
        unexpected_keyword,
        call_site,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicCallableSignatureMismatch {
    callable: String,
    detail: String,
    missing_arguments: Vec<String>,
    call_site: Option<String>,
    source_target: Option<String>,
}

fn public_callable_signature_mismatch(summary: &str) -> Option<PublicCallableSignatureMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || lower.contains("__init__()")
        || !(lower.contains("missing")
            || lower.contains("required positional argument")
            || lower.contains("takes "))
    {
        return None;
    }

    let logical_lines = failure_summary_logical_lines(summary);
    let detail_index = logical_lines.iter().position(|line| {
        let lower_line = line.to_ascii_lowercase();
        lower_line.contains("typeerror:")
            && !lower_line.contains("__init__()")
            && (lower_line.contains("required positional argument")
                || lower_line.contains("positional arguments")
                || lower_line.contains("takes "))
    })?;
    let detail = logical_lines[detail_index]
        .split("TypeError:")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let callable = detail
        .split("()")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    if callable
        .rsplit('.')
        .next()
        .is_some_and(|name| name == "__init__")
    {
        return None;
    }
    let missing_arguments = missing_required_arguments_from_type_error(&detail);
    let call_site = callable_call_site_before(&logical_lines[..detail_index], &callable);
    let source_target = callable_source_target_from_name(&callable);

    Some(PublicCallableSignatureMismatch {
        callable,
        detail,
        missing_arguments,
        call_site,
        source_target,
    })
}

fn missing_required_arguments_from_type_error(detail: &str) -> Vec<String> {
    let mut args = Vec::new();
    for marker in [
        "required positional argument: '",
        "required positional arguments: '",
        "required keyword-only argument: '",
        "required keyword-only arguments: '",
    ] {
        let Some(start) = detail.find(marker).map(|index| index + marker.len()) else {
            continue;
        };
        let rest = &detail[start..];
        let end = rest.find('\'').unwrap_or(rest.len());
        for part in rest[..end].split(" and ") {
            let value = part.trim().trim_matches('\'').trim();
            if !value.is_empty() && !args.iter().any(|existing| existing == value) {
                args.push(value.to_string());
            }
        }
    }
    args
}

fn callable_call_site_before(lines: &[&str], callable: &str) -> Option<String> {
    let terminal = callable.rsplit('.').next().unwrap_or(callable);
    let method_needle = format!(".{terminal}(");
    let function_needle = format!("{terminal}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("typeerror:")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || lower.starts_with("fail:")
            || !trimmed.contains('(')
            || !trimmed.contains(')')
        {
            return None;
        }
        if trimmed.contains(&method_needle) || trimmed.contains(&function_needle) {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

fn callable_source_target_from_name(callable: &str) -> Option<String> {
    let receiver = callable.split('.').next()?.trim();
    if receiver.is_empty()
        || matches!(
            receiver,
            "self" | "cls" | "str" | "int" | "float" | "bool" | "list" | "dict" | "tuple" | "set"
        )
    {
        return None;
    }
    if !receiver.chars().any(|ch| ch.is_ascii_uppercase()) {
        return None;
    }
    let module = upper_camel_to_snake(receiver);
    (!module.is_empty()).then(|| format!("{module}.py"))
}

fn upper_camel_to_snake(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn public_constructor_sibling_data_shape_observations(summary: &str) -> Vec<String> {
    let Some(mismatch) = public_constructor_signature_mismatch(summary) else {
        return Vec::new();
    };
    public_constructor_sibling_data_shape_obligations(summary, &mismatch.constructor)
}

fn public_constructor_sibling_data_shape_obligations(
    summary: &str,
    constructor: &str,
) -> Vec<String> {
    let class_name = constructor.rsplit('.').next().unwrap_or(constructor);
    let mut obligations = Vec::new();
    for attribute in public_missing_attributes(summary) {
        let Some((receiver, _member)) = attribute.split_once('.') else {
            continue;
        };
        if receiver != class_name {
            continue;
        }
        let observation = format!("`{attribute}`");
        if !obligations.iter().any(|existing| existing == &observation) {
            obligations.push(observation);
        }
    }
    obligations
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PublicConstructorBodyExceptionObservation {
    pub constructor_call_site: String,
    pub source_initializer_call: Option<String>,
    pub source_failure_site: Option<String>,
    pub actual_exception: String,
    pub sibling_constructor_obligations: Vec<String>,
}

pub(crate) fn public_constructor_body_exception_observation(
    summary: &str,
) -> Option<PublicConstructorBodyExceptionObservation> {
    public_constructor_body_exception(summary)
}

fn public_constructor_body_exception(
    summary: &str,
) -> Option<PublicConstructorBodyExceptionObservation> {
    let logical_lines = failure_summary_logical_lines(summary);
    if let Some(observation) =
        public_constructor_body_exception_from_public_exception_chain(&logical_lines, summary)
    {
        return Some(observation);
    }
    for (index, line) in logical_lines.iter().enumerate() {
        if !generated_test_frame(line) {
            continue;
        }
        let Some(call_line) = logical_lines.get(index + 1) else {
            continue;
        };
        let Some(constructor_call_site) = public_constructor_body_call_site(call_line) else {
            continue;
        };
        let Some(constructor_name) = public_constructor_name_from_call(&constructor_call_site)
        else {
            continue;
        };
        let search_tail = &logical_lines[index + 2..];
        let Some((source_initializer_call, source_failure_site, actual_exception)) =
            source_constructor_body_exception_after(search_tail)
        else {
            continue;
        };
        let sibling_constructor_obligations =
            public_constructor_signature_obligations(summary, &constructor_name);
        return Some(PublicConstructorBodyExceptionObservation {
            constructor_call_site,
            source_initializer_call,
            source_failure_site,
            actual_exception,
            sibling_constructor_obligations,
        });
    }
    public_constructor_body_exception_from_source_chain(&logical_lines, summary)
}

fn public_constructor_body_exception_from_public_exception_chain(
    logical_lines: &[&str],
    summary: &str,
) -> Option<PublicConstructorBodyExceptionObservation> {
    if !summary.to_ascii_lowercase().contains(" in __init__") {
        return None;
    }
    let init_index = logical_lines
        .iter()
        .position(|line| line.to_ascii_lowercase().contains(" in __init__"))?;
    let constructor_call_site = public_test_constructor_call_site(logical_lines)
        .or_else(|| {
            public_exception_mismatch(summary)
                .and_then(|mismatch| mismatch.call_site)
                .as_deref()
                .and_then(public_constructor_body_call_site)
        })
        .or_else(|| {
            logical_lines[..init_index]
                .iter()
                .find_map(|line| public_constructor_body_call_site(line))
        })
        .unwrap_or_else(|| "public constructor call".to_string());
    let constructor_name = public_constructor_name_from_call(&constructor_call_site)
        .unwrap_or_else(|| constructor_call_site.clone());
    let source_initializer_call = logical_lines
        .get(init_index + 1)
        .map(|value| value.trim())
        .filter(|value| public_constructor_body_code_line(value))
        .map(str::to_string);
    let source_failure_site = logical_lines
        .iter()
        .enumerate()
        .skip(init_index + 1)
        .find_map(|(index, line)| {
            let lower = line.to_ascii_lowercase();
            if !lower.starts_with("file ")
                || !lower.contains(".py")
                || lower.contains("test_")
                || lower.contains("\\python")
                || lower.contains("/python")
                || lower.contains(" in __init__")
            {
                return None;
            }
            logical_lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
        });
    let actual_exception = logical_lines
        .iter()
        .skip(init_index)
        .find(|line| exception_name_from_line(line).is_some())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "constructor body exception".to_string());
    Some(PublicConstructorBodyExceptionObservation {
        constructor_call_site,
        source_initializer_call,
        source_failure_site,
        actual_exception,
        sibling_constructor_obligations: public_constructor_signature_obligations(
            summary,
            &constructor_name,
        ),
    })
}

fn public_test_constructor_call_site(logical_lines: &[&str]) -> Option<String> {
    logical_lines
        .windows(2)
        .find_map(|window| {
            if !generated_test_frame(window[0]) {
                return None;
            }
            public_constructor_body_call_site(window[1])
        })
        .or_else(|| {
            for (index, line) in logical_lines.iter().enumerate() {
                if !generated_test_frame(line) {
                    continue;
                }
                for candidate in logical_lines.iter().skip(index + 1).take(4) {
                    let lower = candidate.to_ascii_lowercase();
                    if lower.trim_start().starts_with("file ") {
                        break;
                    }
                    if let Some(call_site) = public_constructor_body_call_site(candidate) {
                        return Some(call_site);
                    }
                }
            }
            None
        })
        .or_else(|| {
            logical_lines.iter().find_map(|line| {
                let call = public_constructor_body_call_site(line)?;
                let rhs = call
                    .split_once('=')
                    .map(|(_, rhs)| rhs.trim())
                    .unwrap_or(call.as_str());
                rhs.contains('.').then_some(call)
            })
        })
}

fn public_constructor_body_exception_from_source_chain(
    logical_lines: &[&str],
    summary: &str,
) -> Option<PublicConstructorBodyExceptionObservation> {
    for (index, line) in logical_lines.iter().enumerate() {
        if !constructor_init_frame_candidate(line) {
            continue;
        }
        let Some(constructor_call_site) =
            public_exception_call_site_before(&logical_lines[..index])
                .and_then(|line| public_constructor_body_call_site(&line))
                .or_else(|| {
                    logical_lines[..index]
                        .iter()
                        .rev()
                        .find_map(|line| public_constructor_body_call_site(line))
                })
        else {
            continue;
        };
        let Some(constructor_name) = public_constructor_name_from_call(&constructor_call_site)
        else {
            continue;
        };
        let Some((source_initializer_call, source_failure_site, actual_exception)) =
            source_constructor_body_exception_after_relaxed(&logical_lines[index..])
        else {
            continue;
        };
        let sibling_constructor_obligations =
            public_constructor_signature_obligations(summary, &constructor_name);
        return Some(PublicConstructorBodyExceptionObservation {
            constructor_call_site,
            source_initializer_call,
            source_failure_site,
            actual_exception,
            sibling_constructor_obligations,
        });
    }
    public_constructor_body_exception_from_exception_projection(logical_lines, summary)
}

fn public_constructor_body_exception_from_exception_projection(
    logical_lines: &[&str],
    summary: &str,
) -> Option<PublicConstructorBodyExceptionObservation> {
    if !summary.to_ascii_lowercase().contains(" in __init__") {
        return None;
    }
    let mismatch = public_exception_mismatch(summary)?;
    let constructor_call_site = mismatch
        .call_site
        .as_deref()
        .and_then(public_constructor_body_call_site)?;
    let constructor_name = public_constructor_name_from_call(&constructor_call_site)?;
    let source_initializer_call = logical_lines
        .iter()
        .enumerate()
        .find(|(_, line)| constructor_init_frame_candidate(line))
        .and_then(|(index, _)| logical_lines.get(index + 1))
        .map(|value| value.trim())
        .filter(|value| public_constructor_body_code_line(value))
        .map(str::to_string);
    let source_failure_site = mismatch.source_site.as_deref().and_then(|source_site| {
        logical_lines.iter().enumerate().find_map(|(index, line)| {
            if !line.contains(source_site) || line.to_ascii_lowercase().contains(" in __init__") {
                return None;
            }
            logical_lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
        })
    });
    let actual_exception = logical_lines
        .iter()
        .find(|line| exception_name_from_line(line).as_deref() == Some(&mismatch.actual_exception))
        .map(|line| line.trim().to_string())
        .unwrap_or(mismatch.actual_exception);
    let sibling_constructor_obligations =
        public_constructor_signature_obligations(summary, &constructor_name);
    Some(PublicConstructorBodyExceptionObservation {
        constructor_call_site,
        source_initializer_call,
        source_failure_site,
        actual_exception,
        sibling_constructor_obligations,
    })
}

fn local_source_frame_candidate(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    trimmed.starts_with("file ")
        && lower.contains(".py")
        && !lower.contains("test_")
        && !lower.contains("\\python")
        && !lower.contains("/python")
        && !lower.contains("site-packages")
        && !lower.contains("unittest")
}

fn source_constructor_body_exception_after_relaxed(
    lines: &[&str],
) -> Option<(Option<String>, Option<String>, String)> {
    let mut saw_init_frame = false;
    let mut initializer_call = None;
    let mut source_failure_site = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if saw_init_frame && generated_test_frame(trimmed) {
            return None;
        }
        if constructor_init_frame_candidate(trimmed) {
            saw_init_frame = true;
            initializer_call = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string);
            continue;
        }
        if saw_init_frame && local_source_frame_candidate(trimmed) {
            source_failure_site = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
                .or(source_failure_site);
            continue;
        }
        if saw_init_frame && exception_name_from_line(trimmed).is_some() {
            return Some((initializer_call, source_failure_site, trimmed.to_string()));
        }
    }
    None
}

fn constructor_init_frame_candidate(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    trimmed.starts_with("file ")
        && lower.contains(".py")
        && lower.contains(" in __init__")
        && !lower.contains("test_")
        && !lower.contains("site-packages")
        && !lower.contains("unittest")
}

fn source_constructor_body_exception_after(
    lines: &[&str],
) -> Option<(Option<String>, Option<String>, String)> {
    let mut saw_init_frame = false;
    let mut initializer_call = None;
    let mut source_failure_site = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if saw_init_frame && generated_test_frame(line) {
            return None;
        }
        if source_module_frame(line) && line.to_ascii_lowercase().contains(" in __init__") {
            saw_init_frame = true;
            initializer_call = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string);
            index += 1;
            continue;
        }
        if saw_init_frame && source_module_frame(line) {
            source_failure_site = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
                .or(source_failure_site);
            index += 1;
            continue;
        }
        if saw_init_frame && exception_name_from_line(line).is_some() {
            return Some((initializer_call, source_failure_site, line.to_string()));
        }
        index += 1;
    }
    None
}

fn public_constructor_body_call_site(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !public_constructor_body_code_line(trimmed) {
        return None;
    }
    let call = if let Some((_, rhs)) = trimmed.split_once('=') {
        rhs.trim()
    } else {
        trimmed
    };
    let name = public_constructor_name_from_call(call)?;
    if name
        .rsplit('.')
        .next()
        .and_then(|value| value.chars().next())
        .is_some_and(|ch| ch.is_ascii_uppercase())
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn public_constructor_name_from_call(call: &str) -> Option<String> {
    let call = if let Some((_, rhs)) = call.trim().split_once('=') {
        rhs.trim()
    } else {
        call.trim()
    };
    let before_paren = call.split('(').next()?.trim();
    if before_paren.is_empty()
        || before_paren.starts_with("self.")
        || before_paren.starts_with("assert")
    {
        return None;
    }
    Some(before_paren.to_string())
}

fn public_constructor_body_code_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    !line.starts_with("File ")
        && !lower.starts_with("traceback")
        && !lower.starts_with("during handling")
        && !lower.starts_with("error:")
        && !lower.starts_with("failed")
        && !lower.starts_with("raise ")
        && exception_name_from_line(line).is_none()
        && line.contains('(')
        && line.contains(')')
}

fn public_constructor_signature_obligations(summary: &str, main_constructor: &str) -> Vec<String> {
    let mut obligations = Vec::new();
    for line in failure_summary_logical_lines(summary) {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("typeerror:") || !lower.contains(".__init__()") {
            continue;
        }
        let Some(detail) = line.split("TypeError:").nth(1).map(str::trim) else {
            continue;
        };
        let Some(constructor) = detail
            .split(".__init__()")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if constructor == main_constructor {
            continue;
        }
        let observation = format!("`{constructor}.__init__()`: `{detail}`");
        if !obligations.iter().any(|existing| existing == &observation) {
            obligations.push(observation);
        }
    }
    obligations
}

fn generated_test_frame(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    trimmed.starts_with("file ") && lower.contains(".py") && lower.contains("test_")
}

fn source_module_frame(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    trimmed.starts_with("file ")
        && lower.contains(".py")
        && !lower.contains("test_")
        && !lower.contains("\\python")
        && !lower.contains("/python")
        && !lower.contains("site-packages")
        && !lower.contains("unittest")
}

fn constructor_call_site_before(lines: &[&str], constructor: &str) -> Option<String> {
    let class_name = constructor.rsplit('.').next().unwrap_or(constructor);
    let needle = format!("{class_name}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("typeerror:")
            || lower.starts_with("failed")
            || lower.starts_with("error:")
            || lower.starts_with("fail:")
            || !trimmed.contains(&needle)
            || !trimmed.contains(')')
        {
            return None;
        }
        Some(trimmed.to_string())
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicExceptionMismatch {
    actual_exception: String,
    expected_exception: Option<String>,
    call_site: Option<String>,
    source_site: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicExpectedExceptionNotRaised {
    expected_exception: String,
    call_site: Option<String>,
}

fn public_exception_mismatch(summary: &str) -> Option<PublicExceptionMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("traceback") || !lower.contains("test_") {
        return None;
    }
    if let Some(not_raised) = public_expected_exception_not_raised(summary) {
        return Some(PublicExceptionMismatch {
            actual_exception: format!("{} not raised", not_raised.expected_exception),
            expected_exception: Some(not_raised.expected_exception),
            call_site: not_raised.call_site,
            source_site: None,
        });
    }
    let logical_lines = failure_summary_logical_lines(summary);
    let actual_index = logical_lines
        .iter()
        .rposition(|line| exception_name_from_line(line).is_some())?;
    let actual_exception = exception_name_from_line(logical_lines[actual_index])?;
    let expected_exception = expected_public_exception_name_before_actual(
        &logical_lines[..actual_index],
        &actual_exception,
    );
    let call_site = public_exception_call_site_before(&logical_lines[..actual_index]);
    let source_site = public_exception_source_site_before(&logical_lines[..actual_index])?;
    Some(PublicExceptionMismatch {
        actual_exception,
        expected_exception,
        call_site,
        source_site: Some(source_site),
    })
}

fn expected_public_exception_name_before_actual(
    lines: &[&str],
    actual_exception: &str,
) -> Option<String> {
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        if exception_name_from_line(trimmed).is_some() {
            return None;
        }
        known_exception_name_in_text(trimmed).filter(|expected| expected != actual_exception)
    })
}

fn exception_name_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    for exception in known_public_exception_names() {
        if trimmed.starts_with(exception) && trimmed[exception.len()..].starts_with(':') {
            return Some(exception.to_string());
        }
    }
    None
}

fn known_public_exception_names() -> [&'static str; 5] {
    [
        "ZeroDivisionError",
        "ValueError",
        "TypeError",
        "RuntimeError",
        "OverflowError",
    ]
}

fn known_exception_name_in_text(text: &str) -> Option<String> {
    known_public_exception_names()
        .into_iter()
        .find(|exception| text.contains(exception))
        .map(str::to_string)
}

fn public_exception_call_site_before(lines: &[&str]) -> Option<String> {
    for window in lines.windows(2) {
        let frame = window[0].trim();
        let call = window[1].trim();
        if frame.starts_with("File ")
            && frame.to_ascii_lowercase().contains("test")
            && public_exception_call_site_candidate(call)
        {
            return Some(call.to_string());
        }
    }

    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        if !public_exception_call_site_candidate(trimmed) {
            return None;
        }
        Some(trimmed.to_string())
    })
}

fn public_expected_exception_not_raised(summary: &str) -> Option<PublicExpectedExceptionNotRaised> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("traceback")
        || !lower.contains("test_")
        || !lower.contains("assertraises")
        || !lower.contains("assertionerror:")
        || !lower.contains(" not raised")
    {
        return None;
    }
    let logical_lines = failure_summary_logical_lines(summary);
    let expected_exception = logical_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let payload = trimmed.strip_prefix("AssertionError:")?.trim();
        let exception = payload.strip_suffix(" not raised")?.trim();
        (!exception.is_empty()).then(|| exception.to_string())
    })?;
    let call_site = logical_lines.iter().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.contains("assertRaises(") || trimmed.contains("assertRaisesRegex(") {
            Some(trimmed.to_string())
        } else {
            None
        }
    });
    Some(PublicExpectedExceptionNotRaised {
        expected_exception,
        call_site,
    })
}

fn public_exception_source_site_before(lines: &[&str]) -> Option<String> {
    lines.windows(2).rev().find_map(|window| {
        let frame = window[0].trim();
        if !source_module_frame(frame) {
            return None;
        }
        let path = quoted_file_frame_path(frame)?;
        Some(path)
    })
}

fn quoted_file_frame_path(frame: &str) -> Option<String> {
    let trimmed = frame.trim();
    if !trimmed.starts_with("File ") {
        return None;
    }
    let start = frame.find('"')? + 1;
    let rest = &frame[start..];
    let end = rest.find('"')?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn public_exception_call_site_candidate(line: &str) -> bool {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    !trimmed.starts_with("File ")
        && !lower.starts_with("traceback")
        && !lower.starts_with("during handling")
        && !lower.starts_with("error:")
        && !lower.starts_with("failed")
        && !lower.starts_with("raise ")
        && !lower.starts_with("return ")
        && exception_name_from_line(trimmed).is_none()
        && trimmed.contains('(')
        && trimmed.contains(')')
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceParseDefect {
    detail: String,
    path: Option<String>,
    line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceImportTimeNameResolutionDefect {
    pub missing_name: String,
    pub suggested_name: Option<String>,
    pub path: Option<String>,
    pub line: Option<u32>,
}

fn source_parse_defect(summary: &str) -> Option<SourceParseDefect> {
    let logical_lines = failure_summary_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some(detail) = source_parse_defect_detail_from_line(line) else {
            continue;
        };
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..=index]);
        return Some(SourceParseDefect {
            detail,
            path,
            line: line_number,
        });
    }
    None
}

fn source_import_time_name_resolution_defect(
    summary: &str,
) -> Option<SourceImportTimeNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("importerror: failed to import test module")
        || !lower.contains("nameerror:")
        || !lower.contains(" is not defined")
    {
        return None;
    }
    let logical_lines = failure_summary_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some((missing_name, suggested_name)) = source_import_time_name_error_detail(line)
        else {
            continue;
        };
        let (path, line_number) =
            source_import_time_name_resolution_location_before(&logical_lines[..index]);
        if path.is_none() {
            continue;
        }
        return Some(SourceImportTimeNameResolutionDefect {
            missing_name,
            suggested_name,
            path,
            line: line_number,
        });
    }
    None
}

fn generated_test_name_resolution_defect(
    summary: &str,
) -> Option<SourceImportTimeNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("nameerror:") || !lower.contains(" is not defined") {
        return None;
    }
    let logical_lines = failure_summary_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some((missing_name, suggested_name)) = source_import_time_name_error_detail(line)
        else {
            continue;
        };
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..index]);
        if !path.as_deref().is_some_and(target_is_test_like) {
            continue;
        }
        return Some(SourceImportTimeNameResolutionDefect {
            missing_name,
            suggested_name,
            path,
            line: line_number,
        });
    }
    None
}

fn generated_test_reflection_api_misuse(
    summary: &str,
) -> Option<SourceImportTimeNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || !lower.contains("code object was expected, got str")
        || !lower.contains("inspect.getsource(")
        || !lower.contains("__module__")
    {
        return None;
    }
    let logical_lines = failure_summary_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.contains("inspect.getsource(") || !trimmed.contains("__module__") {
            continue;
        }
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..index]);
        if !path.as_deref().is_some_and(target_is_test_like) {
            continue;
        }
        return Some(SourceImportTimeNameResolutionDefect {
            missing_name: "inspect.getsource(__module__ string)".to_string(),
            suggested_name: None,
            path,
            line: line_number,
        });
    }
    None
}

fn source_import_time_name_error_detail(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    if !trimmed.contains("NameError:") || !trimmed.contains(" is not defined") {
        return None;
    }
    let missing_name = extract_quoted_after(trimmed, "NameError: name '")?;
    let suggested_name = extract_quoted_after(trimmed, "Did you mean: '");
    Some((missing_name, suggested_name))
}

fn source_import_time_name_resolution_location_before(
    lines: &[&str],
) -> (Option<String>, Option<u32>) {
    lines
        .iter()
        .rev()
        .filter_map(|line| source_parse_defect_location_from_line(line))
        .find(|(path, _)| {
            path.as_deref()
                .is_some_and(source_import_time_name_resolution_source_frame)
        })
        .unwrap_or((None, None))
}

fn source_import_time_name_resolution_source_frame(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".py")
        && !target_is_test_like(path)
        && !normalized.contains("/python")
        && !normalized.contains("/lib/unittest/")
}

fn source_parse_defect_detail_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    for marker in ["SyntaxError:", "IndentationError:", "TabError:"] {
        if let Some(start) = trimmed.find(marker) {
            return Some(trimmed[start..].trim().to_string());
        }
    }
    None
}

fn source_parse_defect_location_before(lines: &[&str]) -> (Option<String>, Option<u32>) {
    lines
        .iter()
        .rev()
        .find_map(|line| source_parse_defect_location_from_line(line))
        .unwrap_or((None, None))
}

fn source_parse_defect_location_from_line(line: &str) -> Option<(Option<String>, Option<u32>)> {
    let start = line.find("File \"")? + "File \"".len();
    let rest = &line[start..];
    let path_end = rest.find('"')?;
    let path = rest[..path_end].trim();
    let after_path = &rest[path_end..];
    let line_marker = ", line ";
    let line_start = after_path
        .find(line_marker)
        .map(|index| index + line_marker.len());
    let line_number = line_start.and_then(|index| {
        let tail = &after_path[index..];
        let digits = tail
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        digits.parse::<u32>().ok()
    });
    Some(((!path.is_empty()).then(|| path.to_string()), line_number))
}

fn quoted_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in text.chars() {
        if ch == '\'' || ch == '`' {
            if in_quote {
                if !current.trim().is_empty() {
                    segments.push(current.trim().to_string());
                }
                current.clear();
                in_quote = false;
            } else {
                in_quote = true;
            }
            continue;
        }
        if in_quote {
            current.push(ch);
        }
    }
    segments
}

fn extract_quoted_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find('\'')?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn forbidden_tools_for_projection(allowed: &[String]) -> Vec<String> {
    let mut forbidden = Vec::new();
    for tool in ["read", "shell", "todowrite"] {
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
            "content-changing `write` or `apply_patch` to `{exact_target}`"
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
        RepairLaneSubtype::GeneratedTestLoggingContractOverreach => {
            "generated_test_logging_contract_repair"
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
    if matches!(
        subtype,
        RepairLaneSubtype::GeneratedTestSubprocessEncodingMissing
            | RepairLaneSubtype::GeneratedTestSubprocessOutputCaptureMissing
            | RepairLaneSubtype::GeneratedTestLoggingContractOverreach
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
    markers.extend(generated_test_logging_contract_markers(failure_summary));
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

fn generated_test_contract_drift_markers_from_summary(failure_summary: &str) -> Vec<String> {
    let lower = failure_summary.to_ascii_lowercase();
    if !(lower.contains("traceback")
        && lower.contains("self.assertequal(")
        && lower.contains("raise ")
        && lower.contains("error: test_"))
    {
        return Vec::new();
    }
    let has_test_frame = failure_summary_logical_lines(failure_summary)
        .iter()
        .any(|line| line.contains("File \"") && target_is_test_like(line));
    let has_source_raise_frame = failure_summary_logical_lines(failure_summary)
        .windows(2)
        .any(|window| {
            let [frame, code] = window else {
                return false;
            };
            frame.contains("File \"")
                && !target_is_test_like(frame)
                && code.trim_start().starts_with("raise ")
        });
    if has_test_frame && has_source_raise_frame {
        vec![
            "generated-test contract contradiction: test expects a returned value while source raises a public exception".to_string(),
            "generated-test conflict evidence".to_string(),
        ]
    } else {
        Vec::new()
    }
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

fn generated_test_public_output_contract_overreach(
    failure_summary: &str,
) -> Option<PublicOutputStreamAssertionMismatch> {
    let mismatch = public_output_stream_assertion_mismatch(failure_summary)?;
    if mismatch.stream != "stdout" || test_refs_from_summary(failure_summary).is_empty() {
        return None;
    }
    (public_output_values_are_same_scalar_with_decorative_formatting(
        &mismatch.expected_substring,
        &mismatch.observed_value,
    ) || public_output_assertion_is_ungrounded_process_lifecycle(failure_summary, &mismatch))
    .then_some(mismatch)
}

fn public_output_assertion_is_ungrounded_process_lifecycle(
    failure_summary: &str,
    mismatch: &PublicOutputStreamAssertionMismatch,
) -> bool {
    let lower = failure_summary.to_ascii_lowercase();
    if !lower.contains("eof")
        && !lower.contains("end of file")
        && !lower.contains("no input")
        && !lower.contains("empty input")
    {
        return false;
    }
    let expected = clean_output_assertion_value(&mismatch.expected_substring);
    if expected.is_empty() {
        return false;
    }
    let observed = clean_output_assertion_value(&mismatch.observed_value);
    observed.contains(">>>") || observed.is_empty() || !observed.contains(&expected)
}

fn public_output_values_are_same_scalar_with_decorative_formatting(
    expected: &str,
    observed: &str,
) -> bool {
    let expected_scalar = normalize_decorated_public_output_scalar(expected);
    let observed_scalar = normalize_public_output_observed_scalar_for_expected(expected, observed)
        .unwrap_or_else(|| normalize_decorated_public_output_scalar(observed));
    if expected_scalar.is_empty() || observed_scalar.is_empty() || expected == observed {
        return false;
    }
    match (
        expected_scalar.parse::<f64>(),
        observed_scalar.parse::<f64>(),
    ) {
        (Ok(expected), Ok(observed)) => (expected - observed).abs() < f64::EPSILON,
        _ => false,
    }
}

fn normalize_decorated_public_output_scalar(value: &str) -> String {
    let mut trimmed = clean_output_assertion_value(value);
    trimmed = trimmed.trim().to_string();
    if let Some(rest) = trimmed.strip_prefix('=') {
        trimmed = rest.trim().to_string();
    }
    if let Some((_, rest)) = trimmed.rsplit_once(':') {
        let candidate = rest.trim();
        if !candidate.is_empty() {
            trimmed = candidate.to_string();
        }
    }
    trimmed
}

fn normalize_public_output_observed_scalar_for_expected(
    expected: &str,
    observed: &str,
) -> Option<String> {
    let expected = clean_output_assertion_value(expected);
    let observed = clean_output_assertion_value(observed);
    let (label, _) = expected.split_once(':')?;
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let label_start = observed.find(label)?;
    let rest = observed[label_start + label.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix('：'))
        .unwrap_or(rest)
        .trim_start();
    let mut scalar = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch == 'e' || ch == 'E' {
            scalar.push(ch);
        } else if !scalar.is_empty() {
            break;
        }
    }
    (!scalar.is_empty()).then_some(scalar)
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
    obligations.extend(public_state_game_loop_operation_obligations(
        failure_summary,
        public_state_assertions,
    ));
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

fn stable_short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

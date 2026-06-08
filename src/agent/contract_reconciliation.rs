use camino::Utf8PathBuf;

use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target, language_source_targets_from_text,
};
use crate::session::{
    ContractReconciliationDiagnostic, SessionStateSnapshot, VerificationFailureCluster,
    VerificationFailureEvidence,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContractFailureOwner {
    SourceViolatesContract,
    SourceTestContractMismatch,
    TestViolatesContract,
    GeneratedTestOutOfScope,
    ContractInsufficient,
    HarnessInvariantViolation,
    GeneratedTestInsufficient,
    ProviderCapabilityMismatch,
    ToolOrEnvironmentFailure,
    OracleConflict,
}

impl ContractFailureOwner {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SourceViolatesContract => "SourceViolatesContract",
            Self::SourceTestContractMismatch => "SourceTestContractMismatch",
            Self::TestViolatesContract => "TestViolatesContract",
            Self::GeneratedTestOutOfScope => "GeneratedTestOutOfScope",
            Self::ContractInsufficient => "ContractInsufficient",
            Self::HarnessInvariantViolation => "HarnessInvariantViolation",
            Self::GeneratedTestInsufficient => "GeneratedTestInsufficient",
            Self::ProviderCapabilityMismatch => "ProviderCapabilityMismatch",
            Self::ToolOrEnvironmentFailure => "ToolOrEnvironmentFailure",
            Self::OracleConflict => "OracleConflict",
        }
    }

    fn source_repair_allowed(self) -> bool {
        matches!(
            self,
            Self::SourceViolatesContract | Self::SourceTestContractMismatch
        )
    }

    fn test_repair_allowed(self) -> bool {
        matches!(
            self,
            Self::SourceTestContractMismatch
                | Self::TestViolatesContract
                | Self::GeneratedTestOutOfScope
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContractReconciliationDecision {
    pub owner: ContractFailureOwner,
    pub strict_contract_active: bool,
    pub requirement_ids: Vec<String>,
    pub required_target: Option<String>,
    pub source_repair_allowed: bool,
    pub test_repair_allowed: bool,
    pub reason: String,
    pub evidence: Vec<String>,
}

impl ContractReconciliationDecision {
    pub(crate) fn diagnostic(&self) -> ContractReconciliationDiagnostic {
        ContractReconciliationDiagnostic {
            owner: self.owner.as_str().to_string(),
            strict_contract_active: self.strict_contract_active,
            requirement_ids: self.requirement_ids.clone(),
            required_target: self.required_target.clone(),
            source_repair_allowed: self.source_repair_allowed,
            test_repair_allowed: self.test_repair_allowed,
            reason: self.reason.clone(),
            evidence: self.evidence.clone(),
        }
    }

    pub(crate) fn blocks_source_repair(&self) -> bool {
        !self.source_repair_allowed
    }

    pub(crate) fn permits_generated_test_repair(&self) -> bool {
        self.test_repair_allowed
    }

    pub(crate) fn fail_closed(&self) -> bool {
        matches!(
            self.owner,
            ContractFailureOwner::ContractInsufficient
                | ContractFailureOwner::HarnessInvariantViolation
                | ContractFailureOwner::ProviderCapabilityMismatch
                | ContractFailureOwner::ToolOrEnvironmentFailure
                | ContractFailureOwner::OracleConflict
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScenarioContractProfile {
    pub scenario_id: String,
    pub source_target: String,
    pub generated_test_target: Option<String>,
    pub contract_targets: Vec<String>,
    pub requirement_prefixes: Vec<String>,
    pub source_requirement_prefixes: Vec<String>,
    pub test_requirement_prefixes: Vec<String>,
    pub allowed_public_symbols: Vec<String>,
    pub allowed_constructor_keywords: Vec<(String, Vec<String>)>,
}

pub(crate) fn scenario_contract_profile(
    scenario_id: impl Into<String>,
    source_target: impl Into<String>,
    generated_test_target: impl Into<String>,
    contract_targets: Vec<String>,
    allowed_public_symbols: Vec<String>,
    allowed_constructor_keywords: Vec<(String, Vec<String>)>,
) -> ScenarioContractProfile {
    ScenarioContractProfile {
        scenario_id: scenario_id.into(),
        source_target: source_target.into(),
        generated_test_target: Some(generated_test_target.into()),
        contract_targets,
        requirement_prefixes: strings(&[
            "FILE", "API", "STATE", "BEH", "TEST", "VERIFY", "HARNESS",
        ]),
        source_requirement_prefixes: strings(&["FILE", "API", "STATE", "BEH", "VERIFY"]),
        test_requirement_prefixes: strings(&["TEST"]),
        allowed_public_symbols,
        allowed_constructor_keywords,
    }
}

pub(crate) fn generic_scenario_contract_profile(
    active_targets: &[Utf8PathBuf],
    contract_refs: &[Utf8PathBuf],
) -> Option<ScenarioContractProfile> {
    if contract_refs.is_empty() && !has_visible_contract_ref(active_targets) {
        return None;
    }
    let generated_test_target = first_test_target(active_targets);
    let source_target = first_mutable_source_target(active_targets).or_else(|| {
        generated_test_target
            .as_deref()
            .and_then(source_target_for_generated_test_target)
    })?;
    let contract_targets = contract_refs
        .iter()
        .map(|path| file_name(path.as_str()).to_string())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    Some(ScenarioContractProfile {
        scenario_id: "scenario_contract.visible_contract.v1".to_string(),
        source_target,
        generated_test_target,
        contract_targets,
        requirement_prefixes: strings(&[
            "FILE", "API", "STATE", "BEH", "TEST", "VERIFY", "HARNESS",
        ]),
        source_requirement_prefixes: strings(&["FILE", "API", "STATE", "BEH", "VERIFY"]),
        test_requirement_prefixes: strings(&["TEST"]),
        allowed_public_symbols: Vec::new(),
        allowed_constructor_keywords: Vec::new(),
    })
}

pub(crate) fn reconcile_session_state_failure_with_cluster(
    state: &SessionStateSnapshot,
    failure_cluster: Option<&VerificationFailureCluster>,
) -> Option<ContractReconciliationDecision> {
    state.failure.as_ref()?;
    Some(reconcile_failure_with_profile_and_typed_evidence(
        &state.active_targets,
        profile_for_state(state).as_ref(),
        failure_cluster,
        &state.verification.requirement_refs,
    ))
}

pub(crate) fn contract_reconciliation_ignores_diagnostic_label_targets_fixture_passes() -> bool {
    let label_target = "BEH-4: workflow overlap assertion message";
    let active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        None,
        &["BEH-4".to_string()],
    );
    decision.owner == ContractFailureOwner::SourceViolatesContract
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.required_target.as_deref() != Some(label_target)
}

pub(crate) fn contract_reconciliation_source_only_profile_does_not_synthesize_generated_test_target_fixture_passes()
-> bool {
    let active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    let contract_refs = vec![
        Utf8PathBuf::from("scenario_contract.md"),
        Utf8PathBuf::from("scenario_contract.json"),
    ];
    let Some(profile) = generic_scenario_contract_profile(&active_targets, &contract_refs) else {
        return false;
    };
    profile.source_target == "src/workflow.rs" && profile.generated_test_target.is_none()
}

pub(crate) fn generated_test_constructor_misuse_is_test_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-constructor-misuse".to_string(),
        failing_labels: vec!["workflow_generated_test_process_contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --contract".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_constructor_signature_mismatch".to_string()),
            label: Some("workflow_generated_test_process_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("ExternalProcess".to_string()),
            call_site: Some("ExternalProcess(args).with_option(unknown=true)".to_string()),
            exception: Some(
                "ExternalProcess.__init__() got an unexpected keyword argument 'unknown'"
                    .to_string(),
            ),
            expected: None,
            observed: Some("unexpected keyword `unknown`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "ExternalProcess.__init__()".to_string(),
                "constructor keyword compatibility for `ExternalProcess`".to_string(),
                "unexpected keyword `unknown`".to_string(),
                "public_constructor_signature_mismatch".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: vec![
                "constructor keyword compatibility for `ExternalProcess`".to_string(),
                "unexpected keyword `unknown`".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec![
            "constructor keyword compatibility for `ExternalProcess`".to_string(),
            "unexpected keyword `unknown`".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::GeneratedTestOutOfScope
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.permits_generated_test_repair()
        && !decision.source_repair_allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_reconciliation_source_only_profile_does_not_synthesize_generated_test_target() {
        assert!(
            contract_reconciliation_source_only_profile_does_not_synthesize_generated_test_target_fixture_passes()
        );
    }

    #[test]
    fn contract_reconciliation_preserves_workspace_relative_target_identity() {
        assert!(
            contract_reconciliation_preserves_workspace_relative_target_identity_fixture_passes()
        );
    }
}

pub(crate) fn source_constructor_misuse_remains_source_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-source-constructor-misuse".to_string(),
        failing_labels: vec!["workflow_source_constructor_contract".to_string()],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_constructor_signature_mismatch".to_string()),
            label: Some("workflow_source_constructor_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("workflow".to_string()),
            call_site: Some("workflow(invalid=true)".to_string()),
            exception: Some(
                "workflow.__init__() got an unexpected keyword argument 'invalid'".to_string(),
            ),
            expected: None,
            observed: Some("unexpected keyword `invalid`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "workflow.__init__()".to_string(),
                "unexpected keyword `invalid`".to_string(),
                "public_constructor_signature_mismatch".to_string(),
                "workflow-source-contract".to_string(),
            ],
            sibling_obligations: vec!["unexpected keyword `invalid`".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["unexpected keyword `invalid`".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceViolatesContract
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.source_repair_allowed
}

pub(crate) fn generated_test_parse_defect_is_test_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-parse-defect".to_string(),
        failing_labels: vec!["workflow_generated_test_parse_contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --parse".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow_generated_test_parse_contract".to_string()),
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
                "generated_test_artifact_parse_defect".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::TestViolatesContract
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.permits_generated_test_repair()
        && !decision.source_repair_allowed
}

pub(crate) fn source_parse_defect_is_source_owned_without_requirement_id_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let profile = workflow_contract_profile();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-source-parse-defect".to_string(),
        failing_labels: vec!["workflow_source_parse_contract".to_string()],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow_source_parse_contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("source parse defect: missing branch terminator".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `source parse defect: missing branch terminator`".to_string(),
                "source parse frame `src/workflow.rs`".to_string(),
                "source_parse_defect".to_string(),
                "workflow-source-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        Some(&profile),
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceViolatesContract
        && decision.strict_contract_active
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.source_repair_allowed
        && !decision.fail_closed()
}

pub(crate) fn generated_test_name_resolution_self_defect_without_source_public_api_is_test_owned_fixture_passes()
-> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "mixed-generated-test-name-resolution".to_string(),
        failing_labels: vec![
            "workflow_generated_test_helper".to_string(),
            "workflow_source_contract".to_string(),
        ],
        primary_failure: Some(
            "NameError: name 'runtimeEnv' is not defined in generated test helper".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_generated_test_helper".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("runtimeEnv".to_string()),
            call_site: Some("settings = runtimeEnv.current()".to_string()),
            exception: Some("NameError: name 'runtimeEnv' is not defined".to_string()),
            expected: None,
            observed: Some("missing generated-test helper name `runtimeEnv`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated test helper unresolved name".to_string(),
                "generated test artifact name resolution defect".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::TestViolatesContract
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.permits_generated_test_repair()
        && !decision.source_repair_allowed
}

pub(crate) fn generated_test_api_misuse_without_source_public_api_is_test_owned_fixture_passes()
-> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "generated-test-api-misuse".to_string(),
        failing_labels: vec!["workflow_generated_test_api_contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --api".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_generated_test_api_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("reflectContract".to_string()),
            call_site: Some("source = reflectContract(publicWorkflowName)".to_string()),
            exception: Some("TypeError: code object was expected, got contract label".to_string()),
            expected: None,
            observed: Some(
                "generated test invalid reflection subject `reflectContract(contract label)`"
                    .to_string(),
            ),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_artifact_api_misuse".to_string(),
                "generated test invalid reflection subject `reflectContract(contract label)`"
                    .to_string(),
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
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::TestViolatesContract
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.permits_generated_test_repair()
        && !decision.source_repair_allowed
}

pub(crate) fn mixed_generated_test_name_resolution_source_public_api_is_source_test_mismatch_fixture_passes()
-> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "mixed-generated-test-name-resolution-source-public-api".to_string(),
        failing_labels: vec![
            "workflow_generated_test_helper".to_string(),
            "workflow_public_api".to_string(),
        ],
        primary_failure: Some(
            "NameError: name 'runtimeEnv' is not defined in generated test helper".to_string(),
        ),
        evidence: vec![
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("generic_verification_failure".to_string()),
                label: Some("workflow_generated_test_helper".to_string()),
                target: Some("tests/workflow.spec.ts".to_string()),
                symbol: Some("runtimeEnv".to_string()),
                call_site: Some("settings = runtimeEnv.current()".to_string()),
                exception: Some("NameError: name 'runtimeEnv' is not defined".to_string()),
                expected: None,
                observed: Some("missing generated-test helper name `runtimeEnv`".to_string()),
                public_state_assertions: Vec::new(),
                public_missing_attributes: Vec::new(),
                evidence_markers: vec![
                    "generated test helper unresolved name".to_string(),
                    "generated test artifact name resolution defect".to_string(),
                    "workflow-generated-test-contract".to_string(),
                ],
                sibling_obligations: Vec::new(),
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("public_class_attribute_mismatch".to_string()),
                label: Some("workflow_public_api".to_string()),
                target: None,
                symbol: Some("workflow.execute_operation".to_string()),
                call_site: Some("workflow.execute_operation(2, 'add', 3)".to_string()),
                exception: Some(
                    "AttributeError: workflow contract has no public operation `execute_operation`"
                        .to_string(),
                ),
                expected: Some("5".to_string()),
                observed: Some("missing public API".to_string()),
                public_state_assertions: vec![
                    "workflow.execute_operation(2, 'add', 3)".to_string(),
                ],
                public_missing_attributes: vec!["workflow.execute_operation".to_string()],
                evidence_markers: vec![
                    "public_class_attribute_mismatch".to_string(),
                    "public missing method `workflow.execute_operation`".to_string(),
                    "workflow-source-contract".to_string(),
                ],
                sibling_obligations: vec!["`workflow.execute_operation` is missing".to_string()],
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
        ],
        sibling_obligations: vec!["`workflow.execute_operation` is missing".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceTestContractMismatch
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.permits_generated_test_repair()
        && decision.source_repair_allowed
}

pub(crate) fn generated_test_local_binding_contradiction_is_test_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-local-binding-contradiction".to_string(),
        failing_labels: vec!["workflow_generated_test_binding_contract".to_string()],
        primary_failure: Some("Command: verify-generated-test --contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_generated_test_binding_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: Some("first, operation, first = workflow_public_tuple()".to_string()),
            exception: None,
            expected: Some("first public tuple value".to_string()),
            observed: Some("local `first` overwritten by duplicate destructuring".to_string()),
            public_state_assertions: vec!["first".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_local_binding_contradiction".to_string(),
                "generated test local binding contradiction".to_string(),
                "public_state_assertion_mismatch".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: vec!["generated test local binding contradiction".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["generated test local binding contradiction".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::TestViolatesContract
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.permits_generated_test_repair()
        && !decision.source_repair_allowed
}

pub(crate) fn generic_generated_test_only_failure_preserves_active_test_target_fixture_passes()
-> bool {
    let active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generic-generated-test-only".to_string(),
        failing_labels: vec!["workflow_generated_test_visible_contract".to_string()],
        primary_failure: Some("AssertionError: stale literal expectation".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_generated_test_visible_contract".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("old visible literal".to_string()),
            observed: Some("current visible literal".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generic_verification_failure".to_string(),
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
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceTestContractMismatch
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && decision.source_repair_allowed
        && decision.test_repair_allowed
}

pub(crate) fn contract_visible_public_exception_failure_is_source_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let profile = workflow_contract_profile();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-contract-visible-public-exception".to_string(),
        failing_labels: vec!["workflow_invalid_public_input".to_string()],
        primary_failure: Some("workflow public exception behavior was not raised".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_invalid_public_input".to_string()),
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
            sibling_obligations: vec![
                "source_public_behavior_assertion".to_string(),
                "expected public exception `WorkflowError`".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec![
            "source_public_behavior_assertion".to_string(),
            "expected public exception `WorkflowError`".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        Some(&profile),
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceViolatesContract
        && decision.strict_contract_active
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.source_repair_allowed
        && !decision.test_repair_allowed
        && !decision.fail_closed()
}

pub(crate) fn generated_test_exception_type_overreach_is_test_owned_fixture_passes() -> bool {
    let active_targets = workflow_active_targets();
    let profile = workflow_contract_profile();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-exception-type-overreach".to_string(),
        failing_labels: vec!["workflow_generated_test_exception_overreach".to_string()],
        primary_failure: Some("Command: verify-generated-test --contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_generated_test_exception_overreach".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: Some("execute_workflow('invalid')".to_string()),
            exception: Some("WorkflowRecoverableError".to_string()),
            expected: Some("WorkflowError".to_string()),
            observed: Some("WorkflowRecoverableError".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_exception_mismatch".to_string(),
                "generated_test_contract_overreach".to_string(),
                "generated-test exception type assertion overreach".to_string(),
                "workflow-generated-test-contract".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        Some(&profile),
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::TestViolatesContract
        && decision.strict_contract_active
        && decision.required_target.as_deref() == Some("tests/workflow.spec.ts")
        && !decision.source_repair_allowed
        && decision.test_repair_allowed
        && !decision.fail_closed()
}

pub(crate) fn mixed_generated_test_validity_and_public_behavior_preserves_source_test_mismatch_fixture_passes()
-> bool {
    let active_targets = workflow_active_targets();
    let profile = workflow_contract_profile();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-mixed-generated-test-validity-public-behavior".to_string(),
        failing_labels: vec![
            "workflow_generated_test_helper".to_string(),
            "workflow_invalid_public_input".to_string(),
        ],
        primary_failure: Some(
            "NameError: name 'runtimeEnv' is not defined; workflow public exception behavior was not raised"
                .to_string(),
        ),
        evidence: vec![
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("generic_verification_failure".to_string()),
                label: Some("workflow_generated_test_helper".to_string()),
                target: Some("tests/workflow.spec.ts".to_string()),
                symbol: Some("runtimeEnv".to_string()),
                call_site: Some("settings = runtimeEnv.current()".to_string()),
                exception: Some("NameError: name 'runtimeEnv' is not defined".to_string()),
                expected: None,
                observed: Some("missing generated-test helper name `runtimeEnv`".to_string()),
                public_state_assertions: Vec::new(),
                public_missing_attributes: Vec::new(),
                evidence_markers: vec![
                    "generated test helper unresolved name".to_string(),
                    "generated test artifact name resolution defect".to_string(),
                    "workflow-generated-test-contract".to_string(),
                ],
                sibling_obligations: Vec::new(),
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("public_exception_mismatch".to_string()),
                label: Some("workflow_invalid_public_input".to_string()),
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
                sibling_obligations: vec![
                    "source_public_behavior_assertion".to_string(),
                    "expected public exception `WorkflowError`".to_string(),
                ],
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
        ],
        sibling_obligations: vec![
            "source_public_behavior_assertion".to_string(),
            "expected public exception `WorkflowError`".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        Some(&profile),
        Some(&cluster),
        &[],
    );
    decision.owner == ContractFailureOwner::SourceTestContractMismatch
        && decision.strict_contract_active
        && decision.required_target.as_deref() == Some("src/workflow.rs")
        && decision.source_repair_allowed
        && decision.test_repair_allowed
        && !decision.fail_closed()
}

fn workflow_active_targets() -> Vec<Utf8PathBuf> {
    vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ]
}

fn workflow_contract_profile() -> ScenarioContractProfile {
    scenario_contract_profile(
        "scenario_contract.workflow.v1",
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        vec![
            "scenario_contract.md".to_string(),
            "scenario_contract.json".to_string(),
        ],
        vec![
            "execute_workflow".to_string(),
            "workflow.execute_operation".to_string(),
            "render_operation".to_string(),
        ],
        vec![(
            "workflow".to_string(),
            vec!["runtime_config".to_string(), "runtime_settings".to_string()],
        )],
    )
}

pub(crate) fn reconcile_failure_with_profile_and_typed_evidence(
    active_targets: &[Utf8PathBuf],
    profile: Option<&ScenarioContractProfile>,
    failure_cluster: Option<&VerificationFailureCluster>,
    requirement_refs: &[String],
) -> ContractReconciliationDecision {
    let generic_generated_test_only_failure =
        generic_generated_test_only_failure_without_source_evidence(failure_cluster);
    let evidence_stream = ContractEvidenceItemStream::from_typed_evidence(
        active_targets,
        failure_cluster,
        requirement_refs,
        profile,
    );
    let generated_test_target = profile
        .and_then(|profile| profile.generated_test_target.clone())
        .or_else(|| first_test_target(active_targets));
    let source_target = profile
        .map(|profile| profile.source_target.clone())
        .or_else(|| first_mutable_source_target(active_targets))
        .or_else(|| first_source_call_site_target(failure_cluster))
        .or_else(|| {
            generated_test_target
                .as_deref()
                .and_then(source_target_for_generated_test_target)
        });
    let strict_contract_active = profile.is_some();

    reconcile_failure_with_evidence_stream(
        active_targets,
        profile,
        evidence_stream,
        generated_test_target,
        source_target,
        strict_contract_active,
        generic_generated_test_only_failure && requirement_refs.is_empty(),
    )
}

fn reconcile_failure_with_evidence_stream(
    active_targets: &[Utf8PathBuf],
    profile: Option<&ScenarioContractProfile>,
    evidence_stream: ContractEvidenceItemStream,
    generated_test_target: Option<String>,
    source_target: Option<String>,
    strict_contract_active: bool,
    generic_generated_test_only_failure: bool,
) -> ContractReconciliationDecision {
    if evidence_stream.has_classification_marker("provider_capability_mismatch") {
        return decision(
            ContractFailureOwner::ProviderCapabilityMismatch,
            strict_contract_active,
            Vec::new(),
            None,
            "provider metadata or request capability evidence contradicts the route contract",
            vec!["provider capability mismatch marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("harness_invariant_violation") {
        return decision(
            ContractFailureOwner::HarnessInvariantViolation,
            strict_contract_active,
            Vec::new(),
            None,
            "failure belongs to harness invariant classification rather than generated source/test repair",
            vec!["harness invariant marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("tool_or_environment_failure") {
        return decision(
            ContractFailureOwner::ToolOrEnvironmentFailure,
            strict_contract_active,
            Vec::new(),
            None,
            "failure belongs to tool, shell, filesystem, service, or local environment availability rather than generated source/test repair",
            vec!["tool or environment failure marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("oracle_conflict") {
        return decision(
            ContractFailureOwner::OracleConflict,
            strict_contract_active,
            Vec::new(),
            None,
            "scenario contract, generated test, or harness-owned gate verdicts conflict and require contract reconciliation before repair",
            vec!["oracle conflict marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("generated_test_insufficient") {
        return decision_with_target(
            ContractFailureOwner::GeneratedTestInsufficient,
            strict_contract_active,
            Vec::new(),
            generated_test_target.clone(),
            "generated test does not cover one or more scenario contract obligations strongly enough; report coverage before dispatch changes",
            vec!["generated test insufficient coverage marker found".to_string()],
        );
    }

    let requirement_ids = evidence_stream.requirement_ids();

    let generated_test_executable_validity = evidence_stream
        .has_classification_marker("generated_test_artifact_parse_defect")
        || evidence_stream
            .has_classification_marker("generated_test_artifact_name_resolution_defect")
        || evidence_stream.has_classification_marker("generated_test_artifact_api_misuse")
        || evidence_stream.has_classification_marker("generated_test_subprocess_encoding_missing")
        || evidence_stream
            .has_classification_marker("generated_test_subprocess_output_capture_missing");
    if generated_test_executable_validity
        && evidence_stream.has_contract_valid_public_behavior_assertion()
    {
        return decision_with_target(
            ContractFailureOwner::SourceTestContractMismatch,
            strict_contract_active,
            requirement_ids.clone(),
            source_target,
            "verification cluster contains both generated-test executable validity defects and contract-visible source public behavior evidence; preserve mixed ownership instead of collapsing to test-only repair",
            vec![
                "generated test artifact executable validity defect marker found".to_string(),
                "contract-visible public behavior assertion found".to_string(),
            ],
        );
    }
    if generated_test_executable_validity && evidence_stream.has_source_public_callable_obligation()
    {
        return decision_with_target(
            ContractFailureOwner::SourceTestContractMismatch,
            strict_contract_active,
            requirement_ids.clone(),
            source_target,
            "verification cluster contains both generated-test executable validity defects and source-owned public callable obligations; preserve bounded source/test reconciliation instead of collapsing to test-only repair",
            vec![
                "generated test artifact executable validity defect marker found".to_string(),
                "source public callable obligation found".to_string(),
            ],
        );
    }
    if generated_test_executable_validity {
        return decision_with_target(
            ContractFailureOwner::TestViolatesContract,
            strict_contract_active,
            requirement_ids.clone(),
            generated_test_target,
            "generated test artifact has a parse/import/name-resolution/stdout-capture defect and must be repaired as generated-test-owned executable artifact validity",
            vec!["generated test artifact executable validity defect marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("generated_test_contract_overreach") {
        return decision_with_target(
            ContractFailureOwner::TestViolatesContract,
            strict_contract_active,
            requirement_ids.clone(),
            generated_test_target,
            "generated test asserted behavior or side effects beyond the visible source contract and must be repaired as generated-test-owned contract overreach",
            vec!["generated test contract overreach marker found".to_string()],
        );
    }
    if evidence_stream.has_classification_marker("generated_test_local_binding_contradiction") {
        return decision_with_target(
            ContractFailureOwner::TestViolatesContract,
            strict_contract_active,
            requirement_ids.clone(),
            generated_test_target,
            "generated test artifact has a local binding/assertion contradiction and must be repaired as generated-test-owned executable artifact validity",
            vec!["generated test local binding contradiction marker found".to_string()],
        );
    }

    let unlisted_constructor_keyword_evidence =
        evidence_stream.unlisted_constructor_keyword_evidence();
    let generated_test_ownership_evidence = evidence_stream
        .has_classification_marker("generated_test_out_of_scope")
        || evidence_stream.has_unknown_public_symbol_reference()
        || (!requirement_ids.is_empty() && !unlisted_constructor_keyword_evidence.is_empty());

    if let Some(profile) = profile {
        if evidence_stream.should_reconcile_mixed_source_test_cluster(
            active_targets,
            profile,
            generated_test_ownership_evidence,
        ) {
            return decision_with_target(
                ContractFailureOwner::SourceTestContractMismatch,
                strict_contract_active,
                requirement_ids.clone(),
                source_target,
                "verification cluster contains both source-owned scenario-contract failures and generated-test-owned contradictions; preserve both owner surfaces before choosing the exact edit target",
                mixed_source_test_evidence(
                    &requirement_ids,
                    &unlisted_constructor_keyword_evidence,
                ),
            );
        }
    }

    if generated_test_ownership_evidence {
        let evidence =
            if requirement_ids.is_empty() && unlisted_constructor_keyword_evidence.is_empty() {
                vec![
                    "generated test requested a public obligation outside the scenario contract"
                        .to_string(),
                ]
            } else {
                let mut evidence = requirement_ids.clone();
                evidence.extend(unlisted_constructor_keyword_evidence);
                evidence
            };
        return decision_with_target(
            ContractFailureOwner::GeneratedTestOutOfScope,
            strict_contract_active,
            requirement_ids,
            generated_test_target,
            "generated test introduced a public API or behavior not listed in the scenario contract",
            evidence,
        );
    }

    if evidence_stream.has_classification_marker("source_parse_defect_in_source")
        && let Some(target) = source_target.clone()
    {
        return decision_with_target(
            ContractFailureOwner::SourceViolatesContract,
            strict_contract_active,
            requirement_ids.clone(),
            Some(target),
            "source artifact has a typed parse defect; executable source validity repair does not require a scenario requirement id",
            vec!["source parse defect in mutable source artifact".to_string()],
        );
    }

    if strict_contract_active
        && requirement_ids.is_empty()
        && evidence_stream.has_contract_valid_public_behavior_assertion()
        && let Some(target) = source_target.clone()
    {
        return decision_with_target(
            ContractFailureOwner::SourceViolatesContract,
            true,
            Vec::new(),
            Some(target),
            "generated test observed a contract-visible public behavior failure; missing requirement ids do not make the generated-test file the repair target",
            vec![
                "contract-visible public behavior assertion found".to_string(),
                "generated test remains evidence for source behavior".to_string(),
            ],
        );
    }

    if strict_contract_active && requirement_ids.is_empty() {
        return decision(
            ContractFailureOwner::ContractInsufficient,
            true,
            Vec::new(),
            None,
            "verification failure has no scenario contract requirement id, so repair ownership is not authoritative",
            vec!["missing scenario contract requirement id".to_string()],
        );
    }

    if let Some(profile) = profile {
        if evidence_stream.should_reconcile_mixed_source_test_cluster(
            active_targets,
            profile,
            false,
        ) {
            return decision_with_target(
                ContractFailureOwner::SourceTestContractMismatch,
                strict_contract_active,
                requirement_ids.clone(),
                source_target,
                "verification cluster contains multiple scenario-contract behavior requirements and must preserve source/test ownership evidence before choosing the exact edit target",
                requirement_ids,
            );
        }
        if requirement_ids
            .iter()
            .any(|id| prefix_matches(id, &profile.test_requirement_prefixes))
        {
            return decision_with_target(
                ContractFailureOwner::TestViolatesContract,
                strict_contract_active,
                requirement_ids.clone(),
                generated_test_target,
                "generated test failure is owned by the test contract lane",
                requirement_ids,
            );
        }
        if requirement_ids
            .iter()
            .any(|id| prefix_matches(id, &profile.source_requirement_prefixes))
        {
            return decision_with_target(
                ContractFailureOwner::SourceViolatesContract,
                strict_contract_active,
                requirement_ids.clone(),
                source_target,
                "source violates a harness-owned scenario contract requirement",
                requirement_ids,
            );
        }
    }

    if evidence_stream.has_classification_marker("no_tests_ran")
        && let Some(target) = generated_test_target
    {
        return decision_with_target(
            ContractFailureOwner::TestViolatesContract,
            strict_contract_active,
            requirement_ids.clone(),
            Some(target),
            "no tests were collected while a generated-test target is still the active repair surface",
            vec!["no_tests_ran generated test target remains uncollected".to_string()],
        );
    }

    if !strict_contract_active
        && requirement_ids.is_empty()
        && generic_generated_test_only_failure
        && !evidence_stream.has_source_public_callable_obligation()
        && let Some(target) = evidence_scoped_uncontracted_repair_target(
            active_targets,
            generated_test_target.as_deref(),
            source_target.as_deref(),
        )
    {
        return decision_with_target(
            ContractFailureOwner::SourceTestContractMismatch,
            false,
            Vec::new(),
            Some(target),
            "no strict scenario contract or source-owned typed evidence is active; preserve the current item-stream repair target instead of promoting generated-test-only evidence to source-owned repair",
            vec![
                "generic verification failure has no source-owned contract evidence".to_string(),
                "repair target authority remains scoped to current active work".to_string(),
            ],
        );
    }

    decision_with_target(
        ContractFailureOwner::SourceViolatesContract,
        strict_contract_active,
        requirement_ids.clone(),
        source_target,
        "no strict scenario contract is active; existing public behavior repair remains source-owned",
        if requirement_ids.is_empty() {
            vec!["public behavior repair without strict scenario contract".to_string()]
        } else {
            requirement_ids
        },
    )
}

fn generic_generated_test_only_failure_without_source_evidence(
    failure_cluster: Option<&VerificationFailureCluster>,
) -> bool {
    let Some(cluster) = failure_cluster else {
        return false;
    };
    let has_test_ref = cluster
        .test_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.test_refs.iter()),
        )
        .any(|target| target_is_test_like(target));
    if !has_test_ref {
        return false;
    }
    let has_source_ref = cluster
        .source_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.source_refs.iter()),
        )
        .any(|target| target_is_mutable_source_like(target));
    if has_source_ref
        || first_source_call_site_target(Some(cluster)).is_some()
        || has_source_public_callable_obligation_from_cluster(Some(cluster))
        || has_contract_valid_public_behavior_assertion_from_cluster(Some(cluster))
    {
        return false;
    }
    !cluster.evidence.is_empty()
        && cluster.evidence.iter().all(|evidence| {
            evidence.subtype.as_deref() == Some("generic_verification_failure")
                && evidence.public_state_assertions.is_empty()
                && evidence.public_missing_attributes.is_empty()
        })
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

fn evidence_scoped_uncontracted_repair_target(
    active_targets: &[Utf8PathBuf],
    generated_test_target: Option<&str>,
    source_target: Option<&str>,
) -> Option<String> {
    let active_source_target = first_mutable_source_target(active_targets);
    let active_test_target = first_test_target(active_targets);
    active_source_target
        .or(active_test_target)
        .or_else(|| generated_test_target.map(str::to_string))
        .or_else(|| source_target.map(str::to_string))
}

fn profile_for_state(state: &SessionStateSnapshot) -> Option<ScenarioContractProfile> {
    generic_scenario_contract_profile(&state.active_targets, &state.contract_refs)
}

fn decision(
    owner: ContractFailureOwner,
    strict_contract_active: bool,
    requirement_ids: Vec<String>,
    required_target: Option<String>,
    reason: &str,
    evidence: Vec<String>,
) -> ContractReconciliationDecision {
    ContractReconciliationDecision {
        owner,
        strict_contract_active,
        requirement_ids,
        required_target,
        source_repair_allowed: owner.source_repair_allowed(),
        test_repair_allowed: owner.test_repair_allowed(),
        reason: reason.to_string(),
        evidence,
    }
}

fn decision_with_target(
    owner: ContractFailureOwner,
    strict_contract_active: bool,
    requirement_ids: Vec<String>,
    required_target: Option<String>,
    reason: &str,
    evidence: Vec<String>,
) -> ContractReconciliationDecision {
    decision(
        owner,
        strict_contract_active,
        requirement_ids,
        required_target,
        reason,
        evidence,
    )
}

fn has_visible_contract_ref(active_targets: &[Utf8PathBuf]) -> bool {
    active_targets.iter().any(|target| {
        matches!(
            file_name(target.as_str()),
            "scenario_contract.md" | "scenario_contract.json"
        )
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContractEvidenceItemStream {
    items: Vec<ContractEvidenceItem>,
    profile: Option<ScenarioContractProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContractEvidenceItem {
    id: String,
    kind: ContractEvidenceKind,
    requirement_ids: Vec<String>,
    marker: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContractEvidenceKind {
    RequirementRef,
    SourceTracebackFrame,
    SourcePublicBehaviorAssertion,
    GeneratedTestUnknownPublicSymbol,
    GeneratedTestConstructorObligation,
    ClassificationMarker,
}

impl ContractEvidenceItemStream {
    fn from_typed_evidence(
        active_targets: &[Utf8PathBuf],
        failure_cluster: Option<&VerificationFailureCluster>,
        requirement_refs: &[String],
        profile: Option<&ScenarioContractProfile>,
    ) -> Self {
        let mut stream = Self {
            items: Vec::new(),
            profile: profile.cloned(),
        };
        if let Some(profile) = profile.cloned() {
            for requirement_id in typed_requirement_ids(requirement_refs, &profile) {
                stream.push(
                    ContractEvidenceKind::RequirementRef,
                    vec![requirement_id.clone()],
                    requirement_id,
                );
            }
            if failure_cluster.is_some_and(|cluster| {
                cluster_refs_contain(&cluster.source_refs, &profile.source_target)
            }) {
                stream.push(
                    ContractEvidenceKind::SourceTracebackFrame,
                    stream.requirement_ids(),
                    format!("source traceback frame in {}", profile.source_target),
                );
            }
            if has_contract_owned_public_behavior_assertion_from_cluster(
                failure_cluster,
                &profile,
                &stream.requirement_ids(),
            ) {
                stream.push(
                    ContractEvidenceKind::SourcePublicBehaviorAssertion,
                    stream.requirement_ids(),
                    "typed source-owned public behavior/state assertion".to_string(),
                );
            }
            for symbol in typed_unknown_public_symbol_references(failure_cluster, &profile) {
                stream.push(
                    ContractEvidenceKind::GeneratedTestUnknownPublicSymbol,
                    stream.requirement_ids(),
                    format!("unknown public symbol `{symbol}`"),
                );
            }
            for evidence in typed_unlisted_constructor_keyword_evidence(failure_cluster, &profile) {
                stream.push(
                    ContractEvidenceKind::GeneratedTestConstructorObligation,
                    stream.requirement_ids(),
                    evidence,
                );
            }
        }
        if has_source_public_callable_obligation_from_cluster(failure_cluster) {
            stream.push(
                ContractEvidenceKind::SourcePublicBehaviorAssertion,
                Vec::new(),
                "source public callable obligation".to_string(),
            );
        }
        if has_contract_valid_public_behavior_assertion_from_cluster(failure_cluster) {
            stream.push(
                ContractEvidenceKind::SourcePublicBehaviorAssertion,
                Vec::new(),
                "contract-visible public behavior assertion".to_string(),
            );
        }
        for marker in typed_contract_classification_markers(failure_cluster, active_targets) {
            stream.push(
                ContractEvidenceKind::ClassificationMarker,
                Vec::new(),
                marker,
            );
        }
        stream
    }

    fn push(
        &mut self,
        kind: ContractEvidenceKind,
        mut requirement_ids: Vec<String>,
        marker: String,
    ) {
        requirement_ids.sort();
        requirement_ids.dedup();
        let id = format!("contract-evidence-{:04}", self.items.len() + 1);
        self.items.push(ContractEvidenceItem {
            id,
            kind,
            requirement_ids,
            marker,
        });
    }

    fn requirement_ids(&self) -> Vec<String> {
        let mut ids = self
            .items
            .iter()
            .flat_map(|item| item.requirement_ids.iter().cloned())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    fn unlisted_constructor_keyword_evidence(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| item.kind == ContractEvidenceKind::GeneratedTestConstructorObligation)
            .map(|item| item.marker.clone())
            .collect()
    }

    fn has_unknown_public_symbol_reference(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.kind == ContractEvidenceKind::GeneratedTestUnknownPublicSymbol)
    }

    fn has_classification_marker(&self, marker: &str) -> bool {
        self.items.iter().any(|item| {
            item.kind == ContractEvidenceKind::ClassificationMarker && item.marker == marker
        })
    }

    fn has_source_public_callable_obligation(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.kind == ContractEvidenceKind::SourcePublicBehaviorAssertion)
    }

    fn has_contract_valid_public_behavior_assertion(&self) -> bool {
        self.items.iter().any(|item| {
            item.kind == ContractEvidenceKind::SourcePublicBehaviorAssertion
                && item.marker == "contract-visible public behavior assertion"
        })
    }

    fn should_reconcile_mixed_source_test_cluster(
        &self,
        active_targets: &[Utf8PathBuf],
        profile: &ScenarioContractProfile,
        generated_test_ownership_evidence: bool,
    ) -> bool {
        let has_source_target = active_targets
            .iter()
            .any(|target| target_identity_matches(target.as_str(), &profile.source_target));
        let has_generated_test_target =
            profile
                .generated_test_target
                .as_ref()
                .is_some_and(|generated_test_target| {
                    active_targets.iter().any(|target| {
                        target_identity_matches(target.as_str(), generated_test_target)
                    })
                });
        if !has_source_target || !has_generated_test_target {
            return false;
        }
        let requirement_ids = self.requirement_ids();
        let source_requirement_count = requirement_ids
            .iter()
            .filter(|id| prefix_matches(id, &profile.source_requirement_prefixes))
            .count();
        let has_test_requirement = requirement_ids
            .iter()
            .any(|id| prefix_matches(id, &profile.test_requirement_prefixes));
        let has_source_evidence = self.items.iter().any(|item| {
            matches!(
                item.kind,
                ContractEvidenceKind::SourceTracebackFrame
                    | ContractEvidenceKind::SourcePublicBehaviorAssertion
            )
        });
        (source_requirement_count > 0 && has_test_requirement)
            || (generated_test_ownership_evidence
                && source_requirement_count > 1
                && has_source_evidence)
    }
}

fn typed_requirement_ids(
    requirement_refs: &[String],
    profile: &ScenarioContractProfile,
) -> Vec<String> {
    let mut ids = requirement_refs
        .iter()
        .filter(|id| prefix_matches(id, &profile.requirement_prefixes))
        .cloned()
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn cluster_refs_contain(refs: &[String], target: &str) -> bool {
    refs.iter()
        .any(|value| target_identity_matches(value, target))
}

fn has_contract_owned_public_behavior_assertion_from_cluster(
    failure_cluster: Option<&VerificationFailureCluster>,
    profile: &ScenarioContractProfile,
    requirement_ids: &[String],
) -> bool {
    let Some(cluster) = failure_cluster else {
        return false;
    };
    let has_source_requirement = requirement_ids
        .iter()
        .any(|id| prefix_matches(id, &profile.source_requirement_prefixes));
    has_source_requirement
        && cluster
            .sibling_obligations
            .iter()
            .any(|marker| marker == "source_public_behavior_assertion")
}

fn typed_unknown_public_symbol_references(
    failure_cluster: Option<&VerificationFailureCluster>,
    profile: &ScenarioContractProfile,
) -> Vec<String> {
    let Some(cluster) = failure_cluster else {
        return Vec::new();
    };
    let mut symbols = cluster
        .sibling_obligations
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.evidence_markers.iter()),
        )
        .filter_map(|marker| marker.strip_prefix("unknown_public_symbol:"))
        .map(str::trim)
        .filter(|symbol| {
            !profile
                .allowed_public_symbols
                .iter()
                .any(|allowed| allowed == symbol)
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    symbols.sort();
    symbols.dedup();
    symbols
}

fn typed_unlisted_constructor_keyword_evidence(
    failure_cluster: Option<&VerificationFailureCluster>,
    profile: &ScenarioContractProfile,
) -> Vec<String> {
    let Some(cluster) = failure_cluster else {
        return Vec::new();
    };
    let allowed = profile
        .allowed_constructor_keywords
        .iter()
        .map(|(symbol, keywords)| (symbol.as_str(), keywords.as_slice()))
        .collect::<Vec<_>>();
    let mut evidence = cluster
        .sibling_obligations
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.evidence_markers.iter()),
        )
        .filter_map(|marker| marker.strip_prefix("constructor_keyword:"))
        .filter_map(|payload| payload.split_once(':'))
        .filter_map(|(symbol, keyword)| {
            let symbol = symbol.trim();
            let keyword = keyword.trim();
            let listed = allowed.iter().any(|(allowed_symbol, allowed_keywords)| {
                allowed_symbol.eq_ignore_ascii_case(symbol)
                    && allowed_keywords
                        .iter()
                        .any(|allowed_keyword| allowed_keyword.eq_ignore_ascii_case(keyword))
            });
            (!listed).then(|| format!("{symbol}.{keyword}"))
        })
        .collect::<Vec<_>>();
    evidence.sort();
    evidence.dedup();
    evidence
}

fn typed_contract_classification_markers(
    failure_cluster: Option<&VerificationFailureCluster>,
    active_targets: &[Utf8PathBuf],
) -> Vec<String> {
    let Some(cluster) = failure_cluster else {
        return Vec::new();
    };
    let mut markers = cluster
        .evidence
        .iter()
        .flat_map(|evidence| evidence.evidence_markers.iter().cloned())
        .chain(cluster.sibling_obligations.iter().cloned())
        .filter(|marker| {
            matches!(
                marker.as_str(),
                "provider_capability_mismatch"
                    | "harness_invariant_violation"
                    | "tool_or_environment_failure"
                    | "oracle_conflict"
                    | "generated_test_insufficient"
                    | "generated_test_out_of_scope"
                    | "generated_test_artifact_parse_defect"
                    | "generated_test_artifact_name_resolution_defect"
                    | "generated_test_artifact_api_misuse"
                    | "generated_test_subprocess_encoding_missing"
                    | "generated_test_subprocess_output_capture_missing"
                    | "generated_test_contract_overreach"
                    | "generated_test_local_binding_contradiction"
                    | "no_tests_ran"
            )
        })
        .collect::<Vec<_>>();
    if generated_test_constructor_signature_misuse_without_source_target(cluster, active_targets) {
        markers.push("generated_test_out_of_scope".to_string());
    }
    if generated_test_parse_defect_without_source_target(cluster) {
        markers.push("generated_test_artifact_parse_defect".to_string());
    }
    if source_parse_defect_source_target(Some(cluster), None).is_some() {
        markers.push("source_parse_defect_in_source".to_string());
    }
    if generated_test_name_resolution_defect_in_cluster(cluster) {
        markers.push("generated_test_artifact_name_resolution_defect".to_string());
    }
    if generated_test_api_misuse_in_cluster(cluster) {
        markers.push("generated_test_artifact_api_misuse".to_string());
    }
    if generated_test_contract_overreach_in_cluster(cluster) {
        markers.push("generated_test_contract_overreach".to_string());
    }
    if generated_test_local_binding_contradiction_in_cluster(cluster) {
        markers.push("generated_test_local_binding_contradiction".to_string());
    }
    markers.sort();
    markers.dedup();
    markers
}

fn has_source_public_callable_obligation_from_cluster(
    failure_cluster: Option<&VerificationFailureCluster>,
) -> bool {
    let Some(cluster) = failure_cluster else {
        return false;
    };
    cluster.evidence.iter().any(|evidence| {
        !evidence.public_missing_attributes.is_empty()
            || evidence.subtype.as_deref() == Some("public_class_attribute_mismatch")
            || evidence.evidence_markers.iter().any(|marker| {
                let marker = marker.to_ascii_lowercase();
                marker.contains("public missing method")
                    || marker.contains("public missing attribute")
                    || marker.contains("public_class_attribute_mismatch")
            })
    })
}

fn has_contract_valid_public_behavior_assertion_from_cluster(
    failure_cluster: Option<&VerificationFailureCluster>,
) -> bool {
    let Some(cluster) = failure_cluster else {
        return false;
    };
    let has_generated_test_ref = cluster
        .test_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.test_refs.iter()),
        )
        .any(|target| target_is_test_like(target));
    has_generated_test_ref
        && cluster.evidence.iter().any(|evidence| {
            let evidence_has_source_behavior = evidence.subtype.as_deref()
                == Some("public_exception_mismatch")
                || evidence
                    .evidence_markers
                    .iter()
                    .chain(evidence.sibling_obligations.iter())
                    .chain(cluster.sibling_obligations.iter())
                    .any(|marker| {
                        let marker = marker.to_ascii_lowercase();
                        marker == "source_public_behavior_assertion"
                            || marker.contains("source public behavior assertion")
                    });
            evidence_has_source_behavior
                && !evidence_is_generated_test_artifact_validity_defect(evidence)
        })
}

fn evidence_is_generated_test_artifact_validity_defect(
    evidence: &crate::session::VerificationFailureEvidence,
) -> bool {
    evidence_is_parse_defect(evidence)
        || evidence.evidence_markers.iter().any(|marker| {
            generated_test_name_resolution_marker(marker)
                || generated_test_api_misuse_marker(marker)
                || generated_test_subprocess_marker(marker)
        })
}

fn generated_test_local_binding_contradiction_in_cluster(
    cluster: &VerificationFailureCluster,
) -> bool {
    cluster.evidence.iter().any(|evidence| {
        evidence_points_to_generated_test(evidence, cluster)
            && evidence
                .evidence_markers
                .iter()
                .chain(evidence.sibling_obligations.iter())
                .chain(cluster.sibling_obligations.iter())
                .any(|marker| {
                    let marker = marker.to_ascii_lowercase();
                    marker.contains("generated_test_local_binding_contradiction")
                        || marker.contains("generated test local binding contradiction")
                        || marker.contains("generated-test local binding contradiction")
                })
    })
}

fn generated_test_contract_overreach_in_cluster(cluster: &VerificationFailureCluster) -> bool {
    cluster.evidence.iter().any(|evidence| {
        evidence_points_to_generated_test(evidence, cluster)
            && (evidence.subtype.as_deref() == Some("generated_test_logging_contract_overreach")
                || evidence.evidence_markers.iter().any(|marker| {
                    let marker = marker.to_ascii_lowercase();
                    marker.contains("generated_test_logging_contract_overreach")
                        || marker.contains("generated_test_contract_overreach")
                        || marker.contains("generated-test contract overreach")
                        || marker.contains("generated-test logging side-effect assertion")
                }))
    })
}

fn generated_test_name_resolution_defect_in_cluster(cluster: &VerificationFailureCluster) -> bool {
    cluster.evidence.iter().any(|evidence| {
        evidence_points_to_generated_test(evidence, cluster)
            && evidence
                .evidence_markers
                .iter()
                .any(|marker| generated_test_name_resolution_marker(marker))
    })
}

fn generated_test_api_misuse_in_cluster(cluster: &VerificationFailureCluster) -> bool {
    cluster.evidence.iter().any(|evidence| {
        evidence_points_to_generated_test(evidence, cluster)
            && evidence
                .evidence_markers
                .iter()
                .any(|marker| generated_test_api_misuse_marker(marker))
    })
}

fn generated_test_name_resolution_marker(marker: &str) -> bool {
    let marker = marker.to_ascii_lowercase();
    marker.contains("generated test artifact name resolution defect")
        || marker.contains("generated_test_artifact_name_resolution_defect")
        || marker.contains("generated test helper unresolved name")
}

fn generated_test_api_misuse_marker(marker: &str) -> bool {
    let marker = marker.to_ascii_lowercase();
    marker.contains("generated_test_artifact_api_misuse")
        || marker.contains("generated test invalid reflection subject")
}

fn generated_test_subprocess_marker(marker: &str) -> bool {
    let marker = marker.to_ascii_lowercase();
    marker.contains("generated_test_subprocess_encoding_missing")
        || marker.contains("generated test subprocess child encoding missing")
        || marker.contains("generated_test_subprocess_output_capture_missing")
        || marker.contains("subprocess output capture missing")
}

fn generated_test_parse_defect_without_source_target(cluster: &VerificationFailureCluster) -> bool {
    if cluster_has_mutable_source_ref(cluster) {
        return false;
    }
    cluster.evidence.iter().any(|evidence| {
        evidence_is_parse_defect(evidence) && evidence_points_to_generated_test(evidence, cluster)
    })
}

fn evidence_is_parse_defect(evidence: &crate::session::VerificationFailureEvidence) -> bool {
    evidence.subtype.as_deref() == Some("source_parse_defect")
        || evidence.subtype.as_deref() == Some("generated_test_parse_defect")
        || evidence.evidence_markers.iter().any(|marker| {
            marker == "source_parse_defect"
                || marker == "generated_test_parse_defect"
                || marker == "generated_test_artifact_parse_defect"
        })
}

fn source_parse_defect_source_target(
    failure_cluster: Option<&VerificationFailureCluster>,
    source_target: Option<&str>,
) -> Option<String> {
    let cluster = failure_cluster?;
    if !cluster_has_mutable_source_ref(cluster) {
        return None;
    }
    let target = cluster
        .evidence
        .iter()
        .filter(|evidence| evidence_is_parse_defect(evidence))
        .filter_map(|evidence| {
            evidence
                .source_refs
                .iter()
                .chain(evidence.target.iter())
                .find(|target| target_is_mutable_source_like(target))
                .cloned()
        })
        .chain(
            cluster
                .source_refs
                .iter()
                .filter(|target| target_is_mutable_source_like(target))
                .cloned(),
        )
        .next()?;
    source_target
        .filter(|source_target| target_identity_matches(&target, source_target))
        .map(str::to_string)
        .or_else(|| Some(normalized_target_identity(&target)))
}

fn generated_test_constructor_signature_misuse_without_source_target(
    cluster: &VerificationFailureCluster,
    active_targets: &[Utf8PathBuf],
) -> bool {
    if cluster_has_mutable_source_ref(cluster) {
        return false;
    }
    let source_stems = active_targets
        .iter()
        .filter(|target| target_is_mutable_source_like(target.as_str()))
        .filter_map(|target| file_stem(target.as_str()))
        .map(|stem| stem.to_ascii_lowercase())
        .collect::<Vec<_>>();
    cluster.evidence.iter().any(|evidence| {
        evidence_is_constructor_signature_mismatch(evidence)
            && evidence_points_to_generated_test(evidence, cluster)
            && evidence_has_constructor_keyword_misuse(evidence, cluster)
            && !evidence_callable_matches_source_target(evidence, &source_stems)
    })
}

fn cluster_has_mutable_source_ref(cluster: &VerificationFailureCluster) -> bool {
    cluster
        .source_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.source_refs.iter()),
        )
        .chain(
            cluster
                .evidence
                .iter()
                .filter_map(|evidence| evidence.target.as_ref()),
        )
        .any(|target| target_is_mutable_source_like(target))
}

fn evidence_is_constructor_signature_mismatch(
    evidence: &crate::session::VerificationFailureEvidence,
) -> bool {
    evidence.subtype.as_deref() == Some("public_constructor_signature_mismatch")
        || evidence
            .evidence_markers
            .iter()
            .any(|marker| marker == "public_constructor_signature_mismatch")
}

fn evidence_points_to_generated_test(
    evidence: &crate::session::VerificationFailureEvidence,
    cluster: &VerificationFailureCluster,
) -> bool {
    evidence.target.as_deref().is_some_and(target_is_test_like)
        || evidence
            .test_refs
            .iter()
            .any(|target| target_is_test_like(target))
        || cluster
            .test_refs
            .iter()
            .any(|target| target_is_test_like(target))
}

fn evidence_has_constructor_keyword_misuse(
    evidence: &crate::session::VerificationFailureEvidence,
    cluster: &VerificationFailureCluster,
) -> bool {
    evidence
        .evidence_markers
        .iter()
        .chain(evidence.sibling_obligations.iter())
        .chain(cluster.sibling_obligations.iter())
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("unexpected keyword")
                || value.contains("got an unexpected keyword argument")
                || value.contains("constructor keyword compatibility")
        })
}

fn evidence_callable_matches_source_target(
    evidence: &crate::session::VerificationFailureEvidence,
    source_stems: &[String],
) -> bool {
    if source_stems.is_empty() {
        return false;
    }
    let mut callables = Vec::new();
    if let Some(symbol) = evidence.symbol.as_deref() {
        callables.push(symbol.to_ascii_lowercase());
    }
    callables.extend(
        evidence
            .evidence_markers
            .iter()
            .filter_map(|marker| constructor_subject_from_marker(marker))
            .map(|subject| subject.to_ascii_lowercase()),
    );
    if let Some(call_site) = evidence.call_site.as_deref() {
        if let Some(expr) = callable_expr_from_call_site(call_site) {
            callables.push(expr.to_ascii_lowercase());
        }
    }
    callables.iter().any(|callable| {
        source_stems.iter().any(|stem| {
            callable == stem
                || callable.ends_with(&format!(".{stem}"))
                || callable.rsplit('.').next().is_some_and(|last| last == stem)
        })
    })
}

fn constructor_subject_from_marker(marker: &str) -> Option<&str> {
    if let Some((_, rest)) = marker.split_once("constructor keyword compatibility for `") {
        return rest.split_once('`').map(|(subject, _)| subject.trim());
    }
    marker
        .split_once(".__init__")
        .map(|(subject, _)| subject.trim())
        .filter(|subject| !subject.is_empty())
}

fn callable_expr_from_call_site(call_site: &str) -> Option<&str> {
    let (head, _) = call_site.split_once('(')?;
    head.split_whitespace()
        .last()
        .map(str::trim)
        .filter(|expr| !expr.is_empty())
}

fn mixed_source_test_evidence(
    requirement_ids: &[String],
    generated_test_evidence: &[String],
) -> Vec<String> {
    let mut evidence = requirement_ids.to_vec();
    evidence.extend(generated_test_evidence.iter().cloned());
    evidence.push("typed evidence stream preserves source/test sibling evidence".to_string());
    evidence.sort();
    evidence.dedup();
    evidence
}

fn prefix_matches(value: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        value.strip_prefix(prefix).is_some_and(|rest| {
            rest.starts_with('-') && rest[1..].chars().any(|c| c.is_ascii_digit())
        })
    })
}

fn first_test_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .find(|target| target_is_test_like(target.as_str()))
        .map(|target| target.as_str().to_string())
}

fn first_mutable_source_target(targets: &[Utf8PathBuf]) -> Option<String> {
    targets
        .iter()
        .find(|target| target_is_mutable_source_like(target.as_str()))
        .map(|target| target.as_str().to_string())
}

fn source_target_for_generated_test_target(target: &str) -> Option<String> {
    classify_artifact_target(target).source_path
}

fn target_identity_matches(candidate: &str, expected: &str) -> bool {
    normalized_target_identity(candidate)
        .eq_ignore_ascii_case(&normalized_target_identity(expected))
}

fn normalized_target_identity(target: &str) -> String {
    classify_artifact_target(target).normalized_target
}

fn target_is_test_like(target: &str) -> bool {
    classify_artifact_target(target).role == ArtifactRole::Test
}

fn target_is_mutable_source_like(target: &str) -> bool {
    let spec = classify_artifact_target(target);
    let name = file_name(&spec.normalized_target).to_ascii_lowercase();
    spec.role == ArtifactRole::Source
        && !matches!(
            name.as_str(),
            "scenario_contract.md" | "scenario_contract.json"
        )
        && matches!(
            spec.language,
            LanguageFamily::Python | LanguageFamily::Code | LanguageFamily::Unknown
        )
}

fn file_name(target: &str) -> &str {
    target.rsplit(['/', '\\']).next().unwrap_or(target)
}

fn file_stem(target: &str) -> Option<&str> {
    file_name(target)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .or_else(|| Some(file_name(target)))
        .filter(|stem| !stem.is_empty())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub(crate) fn contract_reconciliation_preserves_workspace_relative_target_identity_fixture_passes()
-> bool {
    let active_targets = workflow_active_targets();
    let profile = workflow_contract_profile();
    let mixed_cluster = VerificationFailureCluster {
        cluster_id: "fixture-contract-reconciliation-target-identity".to_string(),
        failing_labels: vec![
            "workflow_source_requirement".to_string(),
            "workflow_test_requirement".to_string(),
        ],
        primary_failure: Some(
            "workflow contract and generated test requirements both failed".to_string(),
        ),
        evidence: Vec::new(),
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let mixed_decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        Some(&profile),
        Some(&mixed_cluster),
        &["BEH-7".to_string(), "TEST-2".to_string()],
    );

    let parse_cluster = VerificationFailureCluster {
        cluster_id: "fixture-source-parse-target-identity".to_string(),
        failing_labels: vec!["workflow_source_parse_contract".to_string()],
        primary_failure: Some("source parse defect in src/workflow.rs".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow_source_parse_contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("source parse defect: missing branch terminator".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["source_parse_defect".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let parse_target =
        source_parse_defect_source_target(Some(&parse_cluster), Some("src/workflow.rs"));

    mixed_decision.owner == ContractFailureOwner::SourceTestContractMismatch
        && mixed_decision.required_target.as_deref() == Some("src/workflow.rs")
        && mixed_decision.source_repair_allowed
        && mixed_decision.test_repair_allowed
        && parse_target.as_deref() == Some("src/workflow.rs")
}

pub(crate) fn contract_reconciliation_cluster_refs_exact_target_identity_fixture_passes() -> bool {
    let exact_refs = vec!["src/workflow.rs".to_string()];
    let sibling_refs = vec!["tests/workflow.rs".to_string()];
    let basename_refs = vec!["workflow.rs".to_string()];

    cluster_refs_contain(&exact_refs, "src/workflow.rs")
        && !cluster_refs_contain(&sibling_refs, "src/workflow.rs")
        && !cluster_refs_contain(&basename_refs, "src/workflow.rs")
}

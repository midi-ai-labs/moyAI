use camino::Utf8PathBuf;

use crate::session::{
    ContractReconciliationDiagnostic, SessionStateSnapshot, VerificationFailureCluster,
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
    pub generated_test_target: String,
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
        generated_test_target: generated_test_target.into(),
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
    let source_target = first_mutable_source_target(active_targets)?;
    let generated_test_target = first_test_target(active_targets)
        .unwrap_or_else(|| format!("test_{}", file_name(&source_target)));
    let contract_targets = contract_refs
        .iter()
        .map(|path| file_name(path.as_str()).to_string())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    Some(scenario_contract_profile(
        "scenario_contract.visible_contract.v1",
        source_target,
        generated_test_target,
        contract_targets,
        Vec::new(),
        Vec::new(),
    ))
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
    let label_target = "BEH-4: bullet overlap assertion message";
    let active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("space_invader.py"),
        Utf8PathBuf::from("test_space_invader.py"),
    ];
    let decision = reconcile_failure_with_profile_and_typed_evidence(
        &active_targets,
        None,
        None,
        &["BEH-4".to_string()],
    );
    decision.owner == ContractFailureOwner::SourceViolatesContract
        && decision.required_target.as_deref() == Some("space_invader.py")
        && decision.required_target.as_deref() != Some(label_target)
}

pub(crate) fn reconcile_failure_with_profile_and_typed_evidence(
    active_targets: &[Utf8PathBuf],
    profile: Option<&ScenarioContractProfile>,
    failure_cluster: Option<&VerificationFailureCluster>,
    requirement_refs: &[String],
) -> ContractReconciliationDecision {
    let evidence_stream =
        ContractEvidenceItemStream::from_typed_evidence(failure_cluster, requirement_refs, profile);
    let generated_test_target = profile
        .map(|profile| profile.generated_test_target.clone())
        .or_else(|| first_test_target(active_targets));
    let source_target = profile
        .map(|profile| profile.source_target.clone())
        .or_else(|| first_mutable_source_target(active_targets));
    let strict_contract_active = profile.is_some();

    reconcile_failure_with_evidence_stream(
        active_targets,
        profile,
        evidence_stream,
        generated_test_target,
        source_target,
        strict_contract_active,
    )
}

fn reconcile_failure_with_evidence_stream(
    active_targets: &[Utf8PathBuf],
    profile: Option<&ScenarioContractProfile>,
    evidence_stream: ContractEvidenceItemStream,
    generated_test_target: Option<String>,
    source_target: Option<String>,
    strict_contract_active: bool,
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
        failure_cluster: Option<&VerificationFailureCluster>,
        requirement_refs: &[String],
        profile: Option<&ScenarioContractProfile>,
    ) -> Self {
        let Some(profile) = profile.cloned() else {
            return Self {
                items: Vec::new(),
                profile: None,
            };
        };
        let mut stream = Self {
            items: Vec::new(),
            profile: Some(profile.clone()),
        };
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
        for marker in typed_contract_classification_markers(failure_cluster) {
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

    fn should_reconcile_mixed_source_test_cluster(
        &self,
        active_targets: &[Utf8PathBuf],
        profile: &ScenarioContractProfile,
        generated_test_ownership_evidence: bool,
    ) -> bool {
        let has_source_target = active_targets
            .iter()
            .any(|target| file_name(target.as_str()).eq_ignore_ascii_case(&profile.source_target));
        let has_generated_test_target = active_targets.iter().any(|target| {
            file_name(target.as_str()).eq_ignore_ascii_case(&profile.generated_test_target)
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
        .any(|value| file_name(value).eq_ignore_ascii_case(target))
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
        .chain(cluster.primary_failure.iter())
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
        .chain(cluster.primary_failure.iter())
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
            )
        })
        .collect::<Vec<_>>();
    markers.sort();
    markers.dedup();
    markers
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

fn target_is_test_like(target: &str) -> bool {
    let name = file_name(target).to_ascii_lowercase();
    name.starts_with("test_") || name.ends_with("_test.py")
}

fn target_is_mutable_source_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    !target_is_test_like(target)
        && !matches!(name, "scenario_contract.md" | "scenario_contract.json")
        && (normalized.contains("/src/")
            || name.ends_with(".py")
            || name.ends_with(".rs")
            || name.ends_with(".js")
            || name.ends_with(".ts")
            || name.ends_with(".tsx")
            || name.ends_with(".jsx"))
}

fn file_name(target: &str) -> &str {
    target.rsplit(['/', '\\']).next().unwrap_or(target)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

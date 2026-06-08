use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use crate::agent::language_evidence::{
    ArtifactRole, classify_artifact_target as classify_language_artifact_target,
};
use crate::agent::prompt::PromptPolicy;
use crate::agent::repair_lane::project_repair_lane;
use crate::agent::state::ActiveWorkContract;
use crate::session::{
    ProcessPhase, RepairControlSnapshotDiagnostic, RepairLaneDiagnostic,
    RepairRecoveryChoiceDiagnostic, SessionStateSnapshot, TaskRoute, TurnDecisionDiagnostic,
    TurnDecisionWarning, TurnDecisionWarningSeverity,
};
pub(crate) fn build_turn_decision_diagnostic(
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
    policy: &PromptPolicy,
    allowed_tools: &BTreeSet<String>,
    tool_choice: Option<String>,
) -> TurnDecisionDiagnostic {
    let mut active_targets = active_work
        .map(active_work_targets)
        .unwrap_or_else(|| state.active_targets.clone());
    if active_targets.is_empty() {
        active_targets = state.active_targets.clone();
    }
    active_targets.sort();
    active_targets.dedup();

    let repair_lane =
        project_repair_lane(state, allowed_tools).map(|projection| projection.diagnostic());
    let active_work_summary = active_work.map(ActiveWorkContract::summary);
    let mut policy_targets = policy
        .execution_focus_targets
        .iter()
        .chain(policy.requested_artifact_targets.iter())
        .chain(policy.documentation_scope_targets.iter())
        .chain(policy.readonly_stall_targets.iter())
        .cloned()
        .collect::<Vec<_>>();
    policy_targets.sort();
    policy_targets.dedup();

    let mut diagnostic = TurnDecisionDiagnostic {
        route: route_label(state.route).to_string(),
        process_phase: process_phase_label(state.process_phase).to_string(),
        active_work_kind: active_work.map(active_work_kind).map(str::to_string),
        active_work_summary,
        active_targets,
        verification_pending: state.completion.verification_pending,
        closeout_ready: state.completion.closeout_ready,
        required_verification_commands: state.verification.required_commands.clone(),
        policy_targets,
        allowed_tools: allowed_tools.iter().cloned().collect(),
        tool_choice,
        warnings: Vec::new(),
        repair_lane,
    };
    diagnostic.warnings = evaluate_turn_decision_projection(&diagnostic);
    diagnostic
}

pub(crate) fn evaluate_turn_decision_projection(
    diagnostic: &TurnDecisionDiagnostic,
) -> Vec<TurnDecisionWarning> {
    projection_warnings(diagnostic)
}

fn projection_warnings(diagnostic: &TurnDecisionDiagnostic) -> Vec<TurnDecisionWarning> {
    let allowed = diagnostic
        .allowed_tools
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut warnings = Vec::new();

    if diagnostic.verification_pending
        && !diagnostic.required_verification_commands.is_empty()
        && !allowed.contains("shell")
        && !(diagnostic.repair_lane.is_some()
            && (allowed.contains("write") || allowed.contains("apply_patch")))
    {
        warnings.push(warning(
            "verification_command_without_shell",
            TurnDecisionWarningSeverity::Error,
            "Verification is pending with exact required command(s), but `shell` is not in the provider-visible tool surface.",
        ));
    }

    if diagnostic.closeout_ready
        && (diagnostic.verification_pending
            || !diagnostic.required_verification_commands.is_empty())
    {
        warnings.push(warning(
            "closeout_ready_with_verification_pending",
            TurnDecisionWarningSeverity::Error,
            "Close-out is marked ready while verification obligations remain visible in state.",
        ));
    }

    if diagnostic
        .tool_choice
        .as_deref()
        .is_some_and(|value| value == "required")
        && diagnostic.allowed_tools.is_empty()
    {
        warnings.push(warning(
            "required_tool_choice_without_tools",
            TurnDecisionWarningSeverity::Error,
            "The provider request requires a tool call while the provider-visible tool surface is empty.",
        ));
    }

    if diagnostic.process_phase == "repair"
        && diagnostic.active_work_kind.as_deref() == Some("verification")
        && diagnostic.verification_pending
        && !diagnostic.active_targets.is_empty()
        && !allowed.contains("write")
        && !allowed.contains("apply_patch")
    {
        warnings.push(warning(
            "repair_required_active_work_without_edit_surface",
            TurnDecisionWarningSeverity::Error,
            "Repair-required active work has open repair targets but the provider-visible tool surface has no content-changing edit tool before verification rerun.",
        ));
    }

    if let Some(repair_lane) = diagnostic.repair_lane.as_ref() {
        let Some(snapshot) = repair_lane.repair_control_snapshot.as_ref() else {
            warnings.push(warning(
                "repair_control_snapshot_missing",
                TurnDecisionWarningSeverity::Warning,
                "Repair lane was admitted without a typed RepairControlSnapshot.",
            ));
            return warnings;
        };
        if snapshot.required_target != repair_lane.required_target {
            warnings.push(warning(
                "repair_control_target_mismatch",
                TurnDecisionWarningSeverity::Warning,
                "RepairControlSnapshot and repair lane disagree on the exact repair target.",
            ));
        }
        if diagnostic.process_phase == "repair"
            && !diagnostic.active_targets.is_empty()
            && repair_lane
                .required_target
                .as_deref()
                .is_some_and(|target| {
                    !diagnostic
                        .active_targets
                        .iter()
                        .any(|active| diagnostic_targets_equivalent(active.as_str(), target))
                })
        {
            warnings.push(warning(
                "repair_target_not_in_active_work_targets",
                TurnDecisionWarningSeverity::Error,
                "Repair lane exact target is outside the current ActiveWork target set.",
            ));
        }
        if let Some(template) = repair_lane.operation_template.as_ref() {
            if template.source_test_ownership == "source"
                && template
                    .exact_target
                    .as_deref()
                    .is_some_and(diagnostic_target_is_test_like)
            {
                warnings.push(warning(
                    "source_owned_repair_generated_test_target",
                    TurnDecisionWarningSeverity::Warning,
                    "A source-owned repair operation cannot dispatch a generated-test exact target.",
                ));
            }
            if template.operation_kind == "source_exception_contract"
                && template
                    .exact_target
                    .as_deref()
                    .is_some_and(diagnostic_target_is_test_like)
            {
                warnings.push(warning(
                    "source_exception_repair_generated_test_target",
                    TurnDecisionWarningSeverity::Warning,
                    "A public exception source repair must keep the source file as the exact repair target.",
                ));
            }
            if !template.required_edit_surface.is_empty()
                && snapshot
                    .hard_invariants
                    .iter()
                    .any(|invariant| invariant == "progress_requires_content_changing_edit")
            {
                let supporting_tools = snapshot
                    .allowed_surface_snapshot
                    .iter()
                    .filter(|tool| {
                        !template
                            .required_edit_surface
                            .iter()
                            .any(|edit_tool| edit_tool == *tool)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !supporting_tools.is_empty() {
                    warnings.push(warning(
                        "repair_control_executable_surface_contains_support_tools",
                        TurnDecisionWarningSeverity::Error,
                        "RepairControlSnapshot requires content-changing edit progress but its executable surface still includes supporting tools.",
                    ));
                }
            }
        }
        if diagnostic.process_phase == "repair"
            && snapshot.selected_recovery_action.starts_with("fail_closed")
        {
            warnings.push(warning(
                "repair_unclassified_before_dispatch",
                TurnDecisionWarningSeverity::Error,
                "Repair lane classification is fail-closed; item-stream authority must decide the provider-visible next action.",
            ));
        }
        if snapshot.recovery_choices.iter().all(|choice| {
            choice.recovery_action != snapshot.selected_recovery_action
                || choice.rollback_depth != snapshot.rollback_depth
        }) {
            warnings.push(warning(
                "repair_control_selected_recovery_missing",
                TurnDecisionWarningSeverity::Warning,
                "RepairControlSnapshot selected a recovery policy that is not present in its recovery choices.",
            ));
        }
        if snapshot
            .hard_invariants
            .iter()
            .all(|invariant| invariant != "progress_requires_content_changing_edit")
        {
            warnings.push(warning(
                "repair_control_progress_invariant_missing",
                TurnDecisionWarningSeverity::Warning,
                "RepairControlSnapshot does not require content-changing repair progress before verification rerun.",
            ));
        }
    }

    warnings
}

fn diagnostic_target_is_test_like(target: &str) -> bool {
    classify_language_artifact_target(target).role == ArtifactRole::Test
}

fn diagnostic_targets_equivalent(left: &str, right: &str) -> bool {
    let left = left.replace('\\', "/").to_ascii_lowercase();
    let right = right.replace('\\', "/").to_ascii_lowercase();
    left == right
}

fn warning(
    code: &str,
    severity: TurnDecisionWarningSeverity,
    message: &str,
) -> TurnDecisionWarning {
    TurnDecisionWarning {
        code: code.to_string(),
        severity,
        message: message.to_string(),
    }
}

fn active_work_kind(contract: &ActiveWorkContract) -> &'static str {
    match contract {
        ActiveWorkContract::RequestedWorkAuthoring { .. } => "requested_work_authoring",
        ActiveWorkContract::DocsRepair { .. } => "docs_repair",
        ActiveWorkContract::Verification { .. } => "verification",
    }
}

fn active_work_targets(contract: &ActiveWorkContract) -> Vec<Utf8PathBuf> {
    match contract {
        ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        } => pending_targets.clone(),
        ActiveWorkContract::DocsRepair { deliverable, .. } => deliverable.iter().cloned().collect(),
        ActiveWorkContract::Verification { targets, .. } => targets.clone(),
    }
}

pub(crate) fn active_work_edit_authority_precedes_verification_rerun_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: workflow.active_work_contract is incomplete".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow.active_work_contract".to_string()];
    state.verification.failure_cluster =
        Some(crate::agent::state::public_class_attribute_cluster_fixture());
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: state.verification.failing_labels.clone(),
        repair_required: true,
        targets: state.active_targets.clone(),
    };
    let allowed_tools = BTreeSet::from(["write".to_string()]);
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &allowed_tools,
        Some("required".to_string()),
    );

    diagnostic.active_targets
        == vec![
            Utf8PathBuf::from("src/workflow.rs"),
            Utf8PathBuf::from("tests/workflow.spec.ts"),
        ]
        && diagnostic
            .repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot
                    .hard_invariants
                    .iter()
                    .any(|value| value == "progress_requires_content_changing_edit")
            })
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.severity != TurnDecisionWarningSeverity::Error)
}

pub(crate) fn repair_lane_target_matches_active_work_authority_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: generated workflow contract stale literal".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow-repair-target-contract".to_string()];
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-turn-decision-repair-target-active-work".to_string(),
        failing_labels: vec!["workflow-repair-target-contract".to_string()],
        primary_failure: Some("AssertionError: stale literal expectation".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow-repair-target-contract".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("old visible literal".to_string()),
            observed: Some("current visible literal".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["workflow-repair-target-contract".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
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
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: state.verification.failing_labels.clone(),
        repair_required: true,
        targets: state.active_targets.clone(),
    };
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &BTreeSet::from(["write".to_string(), "apply_patch".to_string()]),
        Some("required".to_string()),
    );

    diagnostic.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
        && diagnostic
            .repair_lane
            .as_ref()
            .is_some_and(|lane| lane.required_target.as_deref() == Some("tests/workflow.spec.ts"))
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.severity != TurnDecisionWarningSeverity::Error)
}

pub(crate) fn turn_decision_repair_target_exact_path_authority_fixture_passes() -> bool {
    let required_target = "src/workflow.rs".to_string();
    let active_target = "tests/workflow.rs".to_string();
    if diagnostic_targets_equivalent(active_target.as_str(), required_target.as_str()) {
        return false;
    }

    let mut diagnostic = TurnDecisionDiagnostic {
        route: "code".to_string(),
        process_phase: "repair".to_string(),
        active_work_kind: Some("verification".to_string()),
        active_work_summary: Some("Repair source before rerun.".to_string()),
        active_targets: vec![Utf8PathBuf::from(active_target.as_str())],
        verification_pending: true,
        closeout_ready: false,
        required_verification_commands: vec!["verify-contract --behavior".to_string()],
        policy_targets: Vec::new(),
        allowed_tools: vec!["write".to_string()],
        tool_choice: Some("required".to_string()),
        warnings: Vec::new(),
        repair_lane: Some(RepairLaneDiagnostic {
            subtype: "source_public_contract".to_string(),
            required_target: Some(required_target.clone()),
            allowed_tools: vec!["write".to_string()],
            forbidden_tools: Vec::new(),
            missing_symbol: None,
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            contract_reconciliation: None,
            operation_template: None,
            verification_cluster: None,
            repair_intent: None,
            repair_control_snapshot: Some(RepairControlSnapshotDiagnostic {
                admitted: true,
                admission_reason: "typed source repair".to_string(),
                repair_subtype: "source_public_contract".to_string(),
                repair_owner: "source".to_string(),
                selected_recovery_action: "edit_source".to_string(),
                rollback_depth: "bounded".to_string(),
                operation_id: None,
                required_target: Some(required_target),
                allowed_surface_snapshot: vec!["write".to_string()],
                hard_invariants: vec!["progress_requires_content_changing_edit".to_string()],
                recovery_choices: vec![RepairRecoveryChoiceDiagnostic {
                    recovery_action: "edit_source".to_string(),
                    rollback_depth: "bounded".to_string(),
                    allowed_tools: vec!["write".to_string()],
                    required_evidence: Vec::new(),
                    forbidden_directions: Vec::new(),
                    progress_evidence: Vec::new(),
                }],
                forbidden_actions: Vec::new(),
                progress_evidence: Vec::new(),
                verification_rerun_condition: Some(
                    "rerun verify-contract --behavior after source edit".to_string(),
                ),
                verification_cluster_id: Some("workflow-source-contract".to_string()),
            }),
        }),
    };
    diagnostic.warnings = evaluate_turn_decision_projection(&diagnostic);

    diagnostic.warnings.iter().any(|warning| {
        warning.code == "repair_target_not_in_active_work_targets"
            && warning.severity == TurnDecisionWarningSeverity::Error
    })
}

pub(crate) fn post_repair_edit_progress_promotes_shell_rerun_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Verify;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: workflow source was repaired and needs rerun".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow-verification-rerun-contract".to_string()];
    state.verification.failure_cluster =
        Some(crate::agent::state::public_class_attribute_cluster_fixture());
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: state.verification.failing_labels.clone(),
        repair_required: false,
        targets: state.active_targets.clone(),
    };
    let allowed_tools = BTreeSet::from(["shell".to_string()]);
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &allowed_tools,
        Some("required".to_string()),
    );

    diagnostic.allowed_tools == vec!["shell".to_string()]
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.severity != TurnDecisionWarningSeverity::Error)
}

pub(crate) fn post_repair_verify_phase_ignores_stale_unclassified_repair_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Verify;
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "stale verification failure remains until the post-edit rerun".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: Vec::new(),
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let allowed_tools = BTreeSet::from(["shell".to_string()]);
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &allowed_tools,
        Some("required".to_string()),
    );

    diagnostic.process_phase == "verify"
        && diagnostic.allowed_tools == vec!["shell".to_string()]
        && diagnostic.repair_lane.is_some()
        && diagnostic.warnings.iter().all(|warning| {
            warning.code != "repair_unclassified_before_dispatch"
                && warning.severity != TurnDecisionWarningSeverity::Error
        })
}

pub(crate) fn repair_required_active_work_rejects_shell_only_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: workflow public exception contract mismatch".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.implementation_handoff = Some(crate::session::ImplementationHandoff {
        summary: "Continue from typed verification failure.".to_string(),
        completed: Vec::new(),
        remaining: Vec::new(),
        next_actions: Vec::new(),
        target_files: vec![Utf8PathBuf::from("src/workflow.rs")],
        verification_commands: vec!["verify-contract --behavior".to_string()],
        continuation_contract: Some(crate::session::ContinuationContract {
            route: route_label(state.route).to_string(),
            process_phase: process_phase_label(state.process_phase).to_string(),
            active_work_kind: Some("verification".to_string()),
            active_work_summary: Some("Repair src/workflow.rs before rerun.".to_string()),
            target_files: vec![Utf8PathBuf::from("src/workflow.rs")],
            verification_commands: vec!["verify-contract --behavior".to_string()],
            failure_kind: Some("verification_failed".to_string()),
            failure_summary: Some(
                "verification failed: workflow public exception contract mismatch".to_string(),
            ),
            completion_blocker: None,
            invariant_refs: Vec::new(),
            ..crate::session::ContinuationContract::default()
        }),
    });
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: Vec::new(),
        repair_required: true,
        targets: state.active_targets.clone(),
    };
    let allowed_tools = BTreeSet::from(["shell".to_string()]);
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &allowed_tools,
        Some("required".to_string()),
    );

    diagnostic.allowed_tools == vec!["shell".to_string()]
        && diagnostic.warnings.iter().any(|warning| {
            warning.code == "repair_required_active_work_without_edit_surface"
                && warning.severity == TurnDecisionWarningSeverity::Error
        })
}

pub(crate) fn unclassified_repair_fails_closed_before_dispatch_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification command timed out before typed failure classification".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: Vec::new(),
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-generated-test --contract".to_string());
    let active_work = ActiveWorkContract::Verification {
        commands: state.verification.required_commands.clone(),
        failing_labels: Vec::new(),
        repair_required: true,
        targets: Vec::new(),
    };
    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "write".to_string(),
    ]);
    let diagnostic = build_turn_decision_diagnostic(
        &state,
        Some(&active_work),
        &PromptPolicy::default(),
        &allowed_tools,
        Some("auto".to_string()),
    );

    diagnostic
        .repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "unknown"
                && snapshot.selected_recovery_action.starts_with("fail_closed")
        })
        && diagnostic.warnings.iter().any(|warning| {
            warning.code == "repair_unclassified_before_dispatch"
                && warning.severity == TurnDecisionWarningSeverity::Error
        })
}

fn route_label(route: TaskRoute) -> &'static str {
    match route {
        TaskRoute::Code => "code",
        TaskRoute::Docs => "docs",
        TaskRoute::Review => "review",
        TaskRoute::Debug => "debug",
        TaskRoute::Ask => "ask",
        TaskRoute::Summary => "summary",
    }
}

fn process_phase_label(phase: ProcessPhase) -> &'static str {
    match phase {
        ProcessPhase::Discover => "discover",
        ProcessPhase::Author => "author",
        ProcessPhase::Verify => "verify",
        ProcessPhase::Repair => "repair",
        ProcessPhase::Closeout => "closeout",
    }
}

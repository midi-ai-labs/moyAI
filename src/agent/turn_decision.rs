use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use crate::agent::prompt::PromptPolicy;
use crate::agent::repair_lane::project_repair_lane;
use crate::agent::state::ActiveWorkContract;
use crate::session::{
    ProcessPhase, SessionStateSnapshot, TaskRoute, TurnDecisionDiagnostic, TurnDecisionWarning,
    TurnDecisionWarningSeverity,
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
        project_repair_lane(state, allowed_tools, None).map(|projection| projection.diagnostic());
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
        required_next_action: None,
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
        }
        if snapshot.selected_recovery_action.starts_with("fail_closed") {
            warnings.push(warning(
                "repair_unclassified_before_dispatch",
                TurnDecisionWarningSeverity::Warning,
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
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    normalized
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("test_") || name.ends_with("_test.py"))
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: calculator.calculate is missing".to_string(),
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

    diagnostic.required_next_action.is_none()
        && diagnostic.active_targets
            == vec![
                Utf8PathBuf::from("calculator.py"),
                Utf8PathBuf::from("test_calculator.py"),
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

pub(crate) fn post_repair_edit_progress_promotes_shell_rerun_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Verify;
    state.active_targets = vec![Utf8PathBuf::from("test_calculator.py")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: calculator.calculate was repaired and needs rerun"
            .to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("calculator.py")],
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_calculate_add".to_string()];
    state.verification.failure_cluster =
        Some(crate::agent::state::public_class_attribute_cluster_fixture());
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
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

    diagnostic.required_next_action.is_none()
        && diagnostic.allowed_tools == vec!["shell".to_string()]
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.severity != TurnDecisionWarningSeverity::Error)
}

pub(crate) fn repair_required_active_work_ignores_shell_only_continuation_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("calculator.py")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: divide raises wrong public exception".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: vec![Utf8PathBuf::from("calculator.py")],
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.implementation_handoff = Some(crate::session::ImplementationHandoff {
        summary: "Continue from typed verification failure.".to_string(),
        completed: Vec::new(),
        remaining: Vec::new(),
        next_actions: Vec::new(),
        target_files: vec![Utf8PathBuf::from("calculator.py")],
        verification_commands: vec!["python -m unittest".to_string()],
        continuation_contract: Some(crate::session::ContinuationContract {
            route: route_label(state.route).to_string(),
            process_phase: process_phase_label(state.process_phase).to_string(),
            active_work_kind: Some("verification".to_string()),
            active_work_summary: Some("Repair calculator.py before rerun.".to_string()),
            required_next_action: None,
            target_files: vec![Utf8PathBuf::from("calculator.py")],
            verification_commands: vec!["python -m unittest".to_string()],
            failure_kind: Some("verification_failed".to_string()),
            failure_summary: Some(
                "verification failed: divide raises wrong public exception".to_string(),
            ),
            completion_blocker: None,
            invariant_refs: Vec::new(),
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

    diagnostic.required_next_action.is_none()
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

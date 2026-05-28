use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use serde_json::{Value, json};

use crate::agent::completion_guard::completion_workspace_blocked_reason;
use crate::agent::content_shape_contract::python_source_for_test_target;
use crate::agent::prompt::{
    extract_protected_artifact_targets, looks_like_structured_document_work,
    requested_work_contract_from_instruction_text, same_document_update_alias_requested,
    staged_task_artifact_targets_from_text,
};
use crate::agent::verification::{
    canonical_verification_command_identity_key, explicit_verification_commands_from_text,
    verification_command_satisfaction_keys,
};
use crate::protocol::{
    ContentPart, FileChangeEvidence, HistoryItem, HistoryItemId, HistoryItemPayload,
    ToolLifecycleStatus, ToolProgressEffect, TurnId, VerificationRunResult, VerificationRunStatus,
};
use crate::session::{ChangeId, ProjectId, SessionId, SessionRecord, ToolCallId};
use crate::session::{
    CompletionState, ContractStatus, DocsArea, DocsAreaCoverage, DocsDeliverableCoverage,
    DocsDeliverableKind, DocsFactCheck, DocsFactCheckKind, DocsGroundingCoverage,
    DocsGroundingRequirement, DocsPendingDeliverable, DocsRouteState, FailureKind, FailureState,
    MessagePart, MessageRole, ProcessPhase, SessionStateSnapshot, TaskRoute, TodoItem, Transcript,
    VerificationFailureCluster, VerificationFailureEvidence,
};
use crate::tool::ToolName;
use crate::tool::truncate::clip_text_with_ellipsis;

const MAX_VERIFICATION_FAILURE_LABELS: usize = 8;
const MAX_VERIFICATION_FAILURE_DETAIL_LINES: usize = 28;
const MAX_VERIFICATION_FAILURE_DETAIL_CHARS: usize = 2600;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelTurnState {
    pub route: TaskRoute,
    pub process_phase: ProcessPhase,
    pub active_todo: Option<String>,
    pub active_targets: Vec<Utf8PathBuf>,
    pub failure_summary: Option<String>,
    pub verification_summary: Option<String>,
    pub completion_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveWorkContract {
    RequestedWorkAuthoring {
        pending_targets: Vec<Utf8PathBuf>,
        verification_commands: Vec<String>,
    },
    DocsRepair {
        deliverable: Option<Utf8PathBuf>,
        pending_deliverables: Vec<DocsPendingDeliverable>,
        pending_summary: String,
        route_contract_satisfied: bool,
    },
    Verification {
        commands: Vec<String>,
        failing_labels: Vec<String>,
        repair_required: bool,
        targets: Vec<Utf8PathBuf>,
    },
}

impl ActiveWorkContract {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RequestedWorkAuthoring { .. } => "requested_work_authoring",
            Self::DocsRepair { .. } => "docs_repair",
            Self::Verification { .. } => "verification",
        }
    }

    pub fn targets(&self) -> Vec<Utf8PathBuf> {
        match self {
            Self::RequestedWorkAuthoring {
                pending_targets, ..
            } => pending_targets.clone(),
            Self::DocsRepair {
                deliverable,
                pending_deliverables,
                route_contract_satisfied: _,
                ..
            } => {
                let pending = pending_deliverables
                    .iter()
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                if pending.is_empty() {
                    deliverable.iter().cloned().collect()
                } else {
                    pending
                }
            }
            Self::Verification { targets, .. } => targets.clone(),
        }
    }
    pub fn summary(&self) -> String {
        match self {
            Self::RequestedWorkAuthoring {
                pending_targets,
                verification_commands,
            } => {
                let targets = pending_targets
                    .iter()
                    .take(5)
                    .map(|target| format!("`{}`", target.as_str()))
                    .collect::<Vec<_>>();
                let target_summary = if targets.is_empty() {
                    "requested target(s)".to_string()
                } else {
                    targets.join(", ")
                };
                let verification_summary = if verification_commands.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Verification stays blocked until authoring finishes, then run `{}`.",
                        verification_commands.join("`, `")
                    )
                };
                format!(
                    "Requested deliverables still require authoring in the workspace: {target_summary}.{verification_summary}"
                )
            }
            Self::DocsRepair {
                deliverable,
                pending_deliverables,
                pending_summary,
                route_contract_satisfied: _,
            } => {
                let pending = docs_pending_deliverable_contract_summary(pending_deliverables);
                let focus = deliverable
                    .as_ref()
                    .map(|path| format!(" Current selected focus: `{}`.", path.as_str()))
                    .unwrap_or_default();
                if pending.is_empty() {
                    format!("Repair the docs contract.{focus} {pending_summary}")
                } else {
                    format!(
                        "Repair the docs contract across the visible pending deliverable set. {pending}{focus} {pending_summary}"
                    )
                }
            }
            Self::Verification {
                commands,
                failing_labels,
                repair_required,
                targets,
            } => {
                if *repair_required {
                    let target_line = if targets.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Repair the concrete targets first: {}.",
                            targets
                                .iter()
                                .take(5)
                                .map(|target| format!("`{}`", target.as_str()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    if !failing_labels.is_empty() {
                        format!(
                            "Verification repair is active for {}.{target_line}",
                            failing_labels.join(", ")
                        )
                    } else if !commands.is_empty() {
                        format!(
                            "Verification repair is active before rerunning {}.{target_line}",
                            commands.join(", ")
                        )
                    } else {
                        format!("Verification repair is still pending.{target_line}")
                    }
                } else if !commands.is_empty() {
                    format!(
                        "Run the required verification commands before closing out: {}.",
                        commands.join(", ")
                    )
                } else if !failing_labels.is_empty() {
                    format!(
                        "Resolve the missing verification evidence before closing out: {}.",
                        failing_labels.join(", ")
                    )
                } else {
                    "Verification is still pending.".to_string()
                }
            }
        }
    }
}

fn docs_pending_deliverable_contract_summary(pending: &[DocsPendingDeliverable]) -> String {
    if pending.is_empty() {
        return String::new();
    }
    let entries = pending
        .iter()
        .take(5)
        .map(|item| {
            if item.summary.trim().is_empty() {
                format!("`{}`", item.target.as_str())
            } else {
                format!("`{}` ({})", item.target.as_str(), item.summary)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    if pending.len() > 5 {
        format!("Pending docs deliverables: {entries}; ...")
    } else {
        format!("Pending docs deliverables: {entries}.")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RequestedWorkDiscipline {
    required_targets: Vec<Utf8PathBuf>,
    reference_inputs: Vec<Utf8PathBuf>,
    protected_targets: Vec<Utf8PathBuf>,
    verification_commands: Vec<String>,
    pending_targets: Vec<Utf8PathBuf>,
}
pub fn reduce_session_state_from_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    todos: &[TodoItem],
    previous: &SessionStateSnapshot,
) -> SessionStateSnapshot {
    let mut state = previous.clone();
    state = reset_prior_closeout_for_new_user_turn(history_items, state);
    state = apply_typed_item_stream_authority(session, history_items, todos, state);
    state = promote_docs_route_contract_authority(session, history_items, state);
    state = promote_requested_work_authoring_authority(session, history_items, state);
    state = promote_requested_work_verification_authority(session, history_items, state);
    let post_failure_written_targets = observed_written_targets_since_latest_verification_failure(
        history_items,
        session.cwd.as_path(),
    );
    let post_failure_content_progress =
        content_changing_progress_since_latest_verification_failure(history_items);
    let Some(typed_evidence) = latest_typed_verification_failure_context(session, history_items)
    else {
        return state;
    };
    if state.completion.route_contract_pending && state.docs_route.is_some() {
        return apply_docs_route_verification_failure_authority(state, typed_evidence);
    }

    let repair_authority_targets =
        repair_progress_authority_targets(&typed_evidence, session.cwd.as_path());
    let active_repair_target_progress_observed =
        matches!(state.process_phase, ProcessPhase::Repair)
            && state.active_targets.iter().any(|target| {
                observed_target_set_contains_path(
                    &post_failure_written_targets,
                    target,
                    session.cwd.as_path(),
                )
            });
    let any_repair_content_progress_observed =
        !post_failure_written_targets.is_empty() || post_failure_content_progress;
    let source_owned_test_evidence_repair =
        source_owned_repair_targets_include_test_evidence_and_source(&repair_authority_targets)
            && verification_cluster_has_source_owned_generated_test_fallback(
                typed_evidence.failure_cluster.as_ref(),
            );
    let source_owned_source_progress_observed = source_owned_test_evidence_repair
        && repair_authority_targets
            .iter()
            .filter(|target| !is_test_focus_target(target))
            .any(|target| {
                observed_target_set_contains_path(
                    &post_failure_written_targets,
                    target,
                    session.cwd.as_path(),
                )
            });
    let repair_progress_observed = if source_owned_test_evidence_repair {
        source_owned_source_progress_observed
    } else {
        any_repair_content_progress_observed
            || active_repair_target_progress_observed
            || repair_authority_targets.iter().any(|target| {
                observed_target_set_contains_path(
                    &post_failure_written_targets,
                    target,
                    session.cwd.as_path(),
                )
            })
    };
    if source_owned_source_progress_observed {
        state
            .active_targets
            .retain(|target| !is_test_focus_target(target));
    }
    state.process_phase = if repair_progress_observed {
        ProcessPhase::Verify
    } else {
        ProcessPhase::Repair
    };
    if repair_progress_observed {
        state.active_targets.clear();
    } else {
        retain_targets_without_observed_progress(
            &mut state.active_targets,
            &post_failure_written_targets,
            session.cwd.as_path(),
        );
    }
    let remaining_failure_targets = if repair_progress_observed {
        Vec::new()
    } else {
        repair_authority_targets
            .into_iter()
            .filter(|target| {
                if source_owned_source_progress_observed && is_test_focus_target(target) {
                    return false;
                }
                !observed_target_set_contains_path(
                    &post_failure_written_targets,
                    target,
                    session.cwd.as_path(),
                )
            })
            .collect::<Vec<_>>()
    };
    state.active_targets =
        if verification_cluster_has_no_tests_ran(typed_evidence.failure_cluster.as_ref())
            && !remaining_failure_targets.is_empty()
        {
            prioritize_repair_targets(remaining_failure_targets)
        } else {
            verification_failure_repair_targets(state.active_targets, remaining_failure_targets)
        };
    retain_targets_without_observed_progress(
        &mut state.active_targets,
        &post_failure_written_targets,
        session.cwd.as_path(),
    );
    state.failure = Some(typed_evidence.failure.clone());
    state.verification.failing_labels = if typed_evidence.failing_labels.is_empty() {
        extract_verification_failure_labels(&typed_evidence.failure.summary)
    } else {
        typed_evidence.failing_labels
    };
    state.verification.last_evidence_summary = Some(typed_evidence.failure.summary.clone());
    state.verification.failure_cluster = typed_evidence.failure_cluster;
    state.verification.requirement_refs = typed_evidence.requirement_refs;
    merge_required_commands(
        &mut state.verification.required_commands,
        &typed_evidence.required_commands,
    );
    state.completion.open_work_count = 0;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason = Some(format!(
        "verification failed: {}",
        typed_evidence.failure.summary
    ));
    if matches!(state.process_phase, ProcessPhase::Repair)
        && let Some(reconciled_targets) =
            contract_reconciled_verification_repair_targets(&state, session.cwd.as_path())
    {
        state.active_targets = reconciled_targets;
    }
    state
}

fn apply_docs_route_verification_failure_authority(
    mut state: SessionStateSnapshot,
    typed_evidence: TypedVerificationFailureEvidence,
) -> SessionStateSnapshot {
    let mut docs_targets = docs_route_pending_repair_targets(state.docs_route.as_ref());
    if docs_targets.is_empty()
        && let Some(target) = state
            .docs_route
            .as_ref()
            .and_then(|docs| docs.active_deliverable.clone())
    {
        docs_targets.push(target);
    }
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = docs_targets;
    state.failure = Some(typed_evidence.failure.clone());
    state.verification.failing_labels = if typed_evidence.failing_labels.is_empty() {
        extract_verification_failure_labels(&typed_evidence.failure.summary)
    } else {
        typed_evidence.failing_labels
    };
    state.verification.last_evidence_summary = Some(typed_evidence.failure.summary.clone());
    state.verification.failure_cluster = typed_evidence.failure_cluster;
    state.verification.requirement_refs = typed_evidence.requirement_refs;
    merge_required_commands(
        &mut state.verification.required_commands,
        &typed_evidence.required_commands,
    );
    state.completion.open_work_count = 0;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason = Some(format!(
        "verification failed under docs route authority: {}",
        typed_evidence.failure.summary
    ));
    state
}

fn reset_prior_closeout_for_new_user_turn(
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    if !state.completion.closeout_ready {
        return state;
    }
    let Some(latest_user_sequence) = latest_user_turn_sequence(history_items) else {
        return state;
    };
    if !history_items
        .iter()
        .any(|item| history_item_order_scalar(item) < latest_user_sequence)
    {
        return state;
    }
    state.process_phase = ProcessPhase::Discover;
    state.active_targets.clear();
    state.failure = None;
    state.verification.failing_labels.clear();
    state.verification.failure_cluster = None;
    state.verification.last_evidence_summary = None;
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.blocked_reason = None;
    state
}

fn promote_requested_work_authoring_authority(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    if state.completion.route_contract_pending && state.docs_route.is_some() {
        return state;
    }

    let latest_user = latest_user_text_from_history_items(history_items);
    let explicit_required_commands = explicit_required_verification_commands_from_history_items(
        session.cwd.as_path(),
        latest_user.as_deref(),
    );
    let requested_work = requested_work_discipline_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
        &explicit_required_commands,
        None,
    );
    merge_contract_refs(&mut state.contract_refs, &requested_work.reference_inputs);
    if requested_work.pending_targets.is_empty() {
        if let Some(snapshot) = structured_document_summary_snapshot_from_history_items(
            session.cwd.as_path(),
            history_items,
            latest_user.as_deref(),
        ) {
            if !snapshot.missing_files.is_empty() {
                state.process_phase = ProcessPhase::Author;
                state.active_targets = vec![Utf8PathBuf::from(snapshot.output_target.clone())];
                state.completion.open_work_count = 1;
                state.completion.closeout_ready = false;
                state.completion.verification_pending = false;
                state.completion.blocked_reason = Some(format!(
                    "structured document summary is incomplete; remaining source file(s): {}",
                    snapshot
                        .missing_files
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                return state;
            }
        }
        if !requested_work.required_targets.is_empty()
            && requested_work.verification_commands.is_empty()
            && !state.completion.verification_pending
            && !state.completion.route_contract_pending
            && state.failure.is_none()
            && state.verification.failure_cluster.is_none()
        {
            state.process_phase = ProcessPhase::Closeout;
            state.active_targets.clear();
            state.completion.open_work_count = 0;
            state.completion.closeout_ready = true;
            state.completion.verification_pending = false;
            state.completion.blocked_reason = None;
            state.verification.required_commands.clear();
            return state;
        }
        if state.completion.verification_pending
            || state.completion.closeout_ready
            || requested_work_verification_passed(session, history_items)
        {
            return state;
        }
        return state;
    }

    state.process_phase = ProcessPhase::Author;
    state.active_targets = requested_work.pending_targets.clone();
    merge_required_commands(
        &mut state.verification.required_commands,
        &requested_work.verification_commands,
    );
    state.completion.open_work_count = requested_work.pending_targets.len();
    state.completion.closeout_ready = false;
    state.completion.verification_pending = false;
    state.completion.blocked_reason = Some(
        ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: requested_work.pending_targets,
            verification_commands: requested_work.verification_commands,
        }
        .summary(),
    );
    state
}

fn promote_docs_route_contract_authority(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    if state.completion.verification_pending {
        return state;
    }
    let Some(contract) =
        docs_route_contract_from_history_items(session.cwd.as_path(), history_items)
    else {
        return state;
    };
    let latest_user = latest_user_text_from_history_items(history_items);
    let docs_reference_inputs = requested_work_discipline_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
        &[],
        None,
    );
    merge_contract_refs(
        &mut state.contract_refs,
        &docs_reference_inputs.reference_inputs,
    );
    merge_contract_refs(
        &mut state.contract_refs,
        &docs_reference_inputs.protected_targets,
    );
    let docs_route = build_docs_route_state(session.cwd.as_path(), &contract);
    let mut pending_deliverables = docs_route_pending_deliverables_from_parts(
        &docs_route.area_coverage,
        &docs_route.deliverables,
        &docs_route.factual_checks,
        docs_route.active_deliverable.as_ref(),
    );
    if pending_deliverables.is_empty() {
        let deliverable_targets = docs_route
            .deliverables
            .iter()
            .map(|deliverable| deliverable.target.as_str().to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        pending_deliverables.extend(
            docs_reference_inputs
                .pending_targets
                .iter()
                .filter(|target| {
                    is_documentation_target(target.as_path())
                        && deliverable_targets.contains(&target.as_str().to_ascii_lowercase())
                })
                .map(|target| DocsPendingDeliverable {
                    target: target.clone(),
                    summary: "same-document docs update requested after the latest user turn; file-change evidence for this update is not yet observed".to_string(),
                }),
        );
    }
    let pending = !pending_deliverables.is_empty();
    state.route = TaskRoute::Docs;
    state.process_phase = if pending {
        ProcessPhase::Author
    } else {
        ProcessPhase::Verify
    };
    state.active_targets = pending_deliverables
        .iter()
        .map(|item| item.target.clone())
        .collect();
    state.docs_route = Some(DocsRouteState {
        pending_deliverables,
        ..docs_route
    });
    state.completion.route_contract_pending = pending;
    state.completion.route_contract_summary = Some(if pending {
        "docs route contract is pending: survey coverage, deliverable topics, and factual checks must be satisfied from repository evidence".to_string()
    } else {
        "docs route contract satisfied".to_string()
    });
    state.completion.open_work_count = state.active_targets.len();
    state.completion.closeout_ready = !pending;
    state.completion.verification_pending = false;
    state.completion.blocked_reason = pending.then(|| {
        ActiveWorkContract::DocsRepair {
            deliverable: docs_route_pending_repair_target(state.docs_route.as_ref()),
            pending_deliverables: state
                .docs_route
                .as_ref()
                .map(|docs| docs.pending_deliverables.clone())
                .unwrap_or_default(),
            pending_summary: state
                .completion
                .route_contract_summary
                .clone()
                .unwrap_or_else(|| "docs route contract is pending".to_string()),
            route_contract_satisfied: false,
        }
        .summary()
    });
    state
}

fn promote_requested_work_verification_authority(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    if state.completion.verification_pending
        || (state.completion.route_contract_pending && state.docs_route.is_some())
        || requested_work_verification_passed(session, history_items)
    {
        return state;
    }

    let latest_user = latest_user_text_from_history_items(history_items);
    let explicit_required_commands = explicit_required_verification_commands_from_history_items(
        session.cwd.as_path(),
        latest_user.as_deref(),
    );
    if explicit_required_commands.is_empty() {
        return state;
    }

    let requested_work = requested_work_discipline_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
        &explicit_required_commands,
        None,
    );
    if !requested_work.pending_targets.is_empty() {
        return state;
    }

    state.process_phase = ProcessPhase::Verify;
    state.active_targets.clear();
    merge_required_commands(
        &mut state.verification.required_commands,
        &requested_work.verification_commands,
    );
    state.completion.open_work_count = 0;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason = Some(format!(
        "requested work authoring is complete; run required verification command(s): {}",
        state.verification.required_commands.join(", ")
    ));
    state
}

fn merge_required_commands(existing: &mut Vec<String>, additional: &[String]) {
    let mut seen = existing
        .iter()
        .filter_map(|command| canonical_verification_command_identity_key(command))
        .collect::<BTreeSet<_>>();
    for command in additional {
        let key = canonical_verification_command_identity_key(command).unwrap_or_else(|| {
            command
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        });
        if seen.insert(key) {
            existing.push(command.clone());
        }
    }
}

fn merge_contract_refs(existing: &mut Vec<Utf8PathBuf>, additional: &[Utf8PathBuf]) {
    let mut seen = existing
        .iter()
        .map(|target| canonical_target_key(target.as_str()))
        .collect::<BTreeSet<_>>();
    for target in additional {
        if seen.insert(canonical_target_key(target.as_str())) {
            existing.push(target.clone());
        }
    }
}

fn verification_run_satisfaction_keys(run: &VerificationRunResult) -> BTreeSet<String> {
    let mut keys = verification_command_satisfaction_keys(&run.command);
    if let Some(key) = canonical_verification_command_identity_key(&run.command) {
        keys.insert(key);
    }
    keys.extend(
        run.satisfies_command_identities
            .iter()
            .map(|key| key.trim().to_ascii_lowercase())
            .filter(|key| !key.is_empty()),
    );
    keys
}

pub(crate) fn public_verification_command_identity_dedupes_required_commands_fixture_passes() -> bool
{
    let mut required = vec![
        "python -X utf8 component.py 8 +".to_string(),
        "python -X utf8 component.py log 10".to_string(),
    ];
    merge_required_commands(
        &mut required,
        &[
            "python -X utf8 component.py 8 +".to_string(),
            "python -X utf8 component.py log 10".to_string(),
            "python -X utf8 component.py 8 +".to_string(),
        ],
    );

    let run = VerificationRunResult {
        command: "python -X utf8 component.py 8 +".to_string(),
        status: VerificationRunStatus::Passed,
        exit_code: Some(0),
        timed_out: false,
        output_summary: "8".to_string(),
        failure_cluster: None,
        satisfies_command_identities: Vec::new(),
        artifact_refs: Vec::new(),
        requirement_refs: Vec::new(),
    };
    let run_keys = verification_run_satisfaction_keys(&run);

    required.len() == 2
        && run_keys.contains("python -x utf8 component.py 8 +")
        && canonical_verification_command_identity_key(&required[0])
            .is_some_and(|key| run_keys.contains(&key))
}

fn requested_work_verification_passed(
    session: &SessionRecord,
    history_items: &[HistoryItem],
) -> bool {
    let latest_user = latest_user_text_from_history_items(history_items);
    let explicit_required_commands = explicit_required_verification_commands_from_history_items(
        session.cwd.as_path(),
        latest_user.as_deref(),
    );
    if explicit_required_commands.is_empty() {
        return false;
    }
    let latest_content_change_sequence =
        latest_content_change_sequence_since_latest_user(history_items, session.cwd.as_path())
            .unwrap_or_else(|| latest_user_turn_sequence(history_items).unwrap_or(i64::MIN));
    history_items_since_latest_user_turn(history_items)
        .iter()
        .any(|item| {
            let HistoryItemPayload::ToolOutput {
                verification_run: Some(run),
                ..
            } = &item.payload
            else {
                return false;
            };
            if history_item_order_scalar(item) <= latest_content_change_sequence {
                return false;
            }
            matches!(run.status, VerificationRunStatus::Passed) && {
                let run_keys = verification_run_satisfaction_keys(run);
                explicit_required_commands.iter().any(|required| {
                    canonical_verification_command_identity_key(required)
                        .is_some_and(|required_key| run_keys.contains(&required_key))
                })
            }
        })
}

fn latest_content_change_sequence_since_latest_user(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> Option<i64> {
    history_items_since_latest_user_turn(history_items)
        .into_iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::FileChange { changes, .. }
                if file_changes_have_authoring_content_change(changes, workspace_root) =>
            {
                Some(history_item_order_scalar(item))
            }
            HistoryItemPayload::ToolOutput { metadata, .. }
                if metadata_has_authoring_content_change(metadata, workspace_root) =>
            {
                Some(history_item_order_scalar(item))
            }
            _ => None,
        })
        .max()
}

fn apply_typed_item_stream_authority(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    _todos: &[TodoItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    let workspace_root = session.cwd.as_path();
    let mut observed_written_targets = BTreeSet::new();
    let mut verification_passed = false;
    let mut passed_verification_command_keys = BTreeSet::new();
    let latest_content_change_sequence =
        latest_content_change_sequence_since_latest_user(history_items, workspace_root);

    for item in history_items_since_latest_user_turn(history_items) {
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref()) {
                        if let Some(normalized) =
                            normalize_target_path(path.as_str(), workspace_root)
                                .filter(|path| is_authoring_content_change_path(path.as_path()))
                        {
                            observed_written_targets.insert(normalized);
                        }
                    }
                }
            }
            HistoryItemPayload::ToolOutput {
                success,
                progress_effect,
                metadata,
                verification_run,
                ..
            } => {
                for path in changed_paths_from_tool_output_metadata(metadata) {
                    if let Some(normalized) = normalize_target_path(&path, workspace_root)
                        .filter(|path| is_authoring_content_change_path(path.as_path()))
                    {
                        observed_written_targets.insert(normalized);
                    }
                }
                if latest_content_change_sequence
                    .is_some_and(|sequence| history_item_order_scalar(item) <= sequence)
                {
                    continue;
                }
                if *success == Some(true)
                    && matches!(
                        progress_effect,
                        crate::protocol::ToolProgressEffect::VerificationPassed
                            | crate::protocol::ToolProgressEffect::MadeProgress
                    )
                    && verification_run
                        .as_ref()
                        .is_some_and(|run| matches!(run.status, VerificationRunStatus::Passed))
                {
                    verification_passed = true;
                    if let Some(run) = verification_run {
                        passed_verification_command_keys
                            .extend(verification_run_satisfaction_keys(run));
                    }
                }
            }
            _ => {}
        }
    }

    if !observed_written_targets.is_empty() {
        state.active_targets.retain(|target| {
            normalize_target_path(target.as_str(), workspace_root)
                .is_none_or(|target| !observed_written_targets.contains(&target))
        });
        if let Some(handoff) = state.implementation_handoff.as_mut() {
            handoff.target_files.retain(|target| {
                normalize_target_path(target.as_str(), workspace_root)
                    .is_none_or(|target| !observed_written_targets.contains(&target))
            });
            handoff.remaining.retain(|item| {
                !observed_written_targets
                    .iter()
                    .any(|target| item.contains(target.as_str()))
            });
        }
    }

    if verification_passed {
        state.failure = None;
        state.verification.failing_labels.clear();
        state.verification.failure_cluster = None;
        state.verification.last_evidence_summary = None;
        if !passed_verification_command_keys.is_empty() {
            state.verification.required_commands.retain(|command| {
                canonical_verification_command_identity_key(command)
                    .is_none_or(|key| !passed_verification_command_keys.contains(&key))
            });
        }
        let verification_obligations_remain = !state.verification.required_commands.is_empty()
            || !state.verification.failing_labels.is_empty()
            || state.verification.failure_cluster.is_some();
        state.completion.verification_pending = verification_obligations_remain;
        state.completion.blocked_reason = None;
        if state.active_targets.is_empty() && !verification_obligations_remain {
            state.completion.open_work_count = 0;
            state.completion.closeout_ready = true;
            state.process_phase = ProcessPhase::Closeout;
        } else if verification_obligations_remain {
            state.completion.closeout_ready = false;
            state.process_phase = ProcessPhase::Verify;
        }
    }

    state
}

fn history_items_in_sequence(history_items: &[HistoryItem]) -> Vec<&HistoryItem> {
    let mut ordered = history_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| (history_item_order_scalar(item), item.sequence_no));
    ordered
}

fn history_items_since_latest_user_turn(history_items: &[HistoryItem]) -> Vec<&HistoryItem> {
    let Some(latest_user_sequence) = latest_user_turn_sequence(history_items) else {
        return history_items_in_sequence(history_items);
    };
    history_items_in_sequence(history_items)
        .into_iter()
        .filter(|item| history_item_order_scalar(item) >= latest_user_sequence)
        .collect()
}

fn latest_user_turn_sequence(history_items: &[HistoryItem]) -> Option<i64> {
    history_items
        .iter()
        .filter(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
        })
        .map(history_item_order_scalar)
        .max()
}

fn history_item_order_scalar(item: &HistoryItem) -> i64 {
    if item.created_at_ms > 0 {
        item.created_at_ms.saturating_mul(1_000_000) + item.sequence_no
    } else {
        item.sequence_no
    }
}

pub fn project_model_turn_state(
    state: &SessionStateSnapshot,
    _todos: &[TodoItem],
) -> ModelTurnState {
    let verification_failure = matches!(
        state.failure.as_ref().map(|value| value.kind),
        Some(FailureKind::VerificationFailed)
    );
    let verification_focus = verification_failure
        || matches!(
            state.process_phase,
            ProcessPhase::Verify | ProcessPhase::Repair
        );
    ModelTurnState {
        route: state.route,
        process_phase: state.process_phase,
        active_todo: None,
        active_targets: state.active_targets.clone(),
        failure_summary: state.failure.as_ref().map(|value| value.summary.clone()),
        verification_summary: if !verification_focus
            || (state.verification.failing_labels.is_empty()
                && state.verification.required_commands.is_empty())
        {
            None
        } else if !state.verification.failing_labels.is_empty() {
            Some(format!(
                "Missing successful {}.",
                state.verification.failing_labels.join(", ")
            ))
        } else {
            Some(format!(
                "Run {} before completion.",
                state.verification.required_commands.join(", ")
            ))
        },
        completion_summary: if let Some(reason) = &state.completion.blocked_reason {
            Some(reason.clone())
        } else if let Some(summary) = &state.completion.route_contract_summary {
            Some(summary.clone())
        } else {
            None
        },
    }
}

fn state_native_active_work_contract(state: &SessionStateSnapshot) -> Option<ActiveWorkContract> {
    if state.completion.route_contract_pending && state.docs_route.is_some() {
        let pending_summary = state
            .completion
            .route_contract_summary
            .clone()
            .or_else(|| state.completion.blocked_reason.clone())
            .unwrap_or_else(|| "Docs route contract is still pending.".to_string());
        return Some(ActiveWorkContract::DocsRepair {
            deliverable: docs_route_pending_repair_target(state.docs_route.as_ref()),
            pending_deliverables: docs_route_pending_deliverables_from_state(
                state.docs_route.as_ref(),
            ),
            pending_summary,
            route_contract_satisfied: false,
        });
    }

    if state.completion.verification_pending {
        return Some(ActiveWorkContract::Verification {
            commands: state.verification.required_commands.clone(),
            failing_labels: state.verification.failing_labels.clone(),
            repair_required: matches!(state.process_phase, ProcessPhase::Repair)
                && matches!(
                    state.failure.as_ref().map(|failure| failure.kind),
                    Some(FailureKind::VerificationFailed | FailureKind::PatchMismatch)
                ),
            targets: state.active_targets.clone(),
        });
    }

    None
}

pub(crate) fn active_work_contract_for_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    _todos: &[TodoItem],
) -> Option<ActiveWorkContract> {
    let latest_user = latest_user_text_from_history_items(history_items);
    let explicit_required_commands = explicit_required_verification_commands_from_history_items(
        session.cwd.as_path(),
        latest_user.as_deref(),
    );
    let workspace_blocked_reason =
        completion_workspace_blocked_reason(&session.cwd, latest_user.as_deref());
    let requested_work = requested_work_discipline_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
        &explicit_required_commands,
        workspace_blocked_reason.as_deref(),
    );

    if state.completion.route_contract_pending && state.docs_route.is_some() {
        return state_native_active_work_contract(state);
    }

    if let Some(snapshot) = structured_document_summary_snapshot_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
    ) {
        if !snapshot.missing_files.is_empty() {
            return Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets: vec![Utf8PathBuf::from(snapshot.output_target)],
                verification_commands: Vec::new(),
            });
        }
    }

    if state.completion.verification_pending && matches!(state.process_phase, ProcessPhase::Verify)
    {
        return state_native_active_work_contract(state);
    }

    if state.completion.verification_pending
        && matches!(state.process_phase, ProcessPhase::Repair)
        && matches!(
            state.failure.as_ref().map(|failure| failure.kind),
            Some(FailureKind::VerificationFailed | FailureKind::PatchMismatch)
        )
    {
        return Some(ActiveWorkContract::Verification {
            commands: verification_commands_for_active_repair(state, &requested_work),
            failing_labels: state.verification.failing_labels.clone(),
            repair_required: true,
            targets: contract_reconciled_verification_repair_targets(state, session.cwd.as_path())
                .or_else(|| verification_repair_targets_from_state(state))
                .unwrap_or_else(|| state.active_targets.clone()),
        });
    }

    if !requested_work.pending_targets.is_empty() {
        return Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: requested_work.pending_targets,
            verification_commands: requested_work.verification_commands,
        });
    }

    if !requested_work.required_targets.is_empty()
        && !requested_work.verification_commands.is_empty()
        && !requested_work_verification_passed(session, history_items)
    {
        return Some(ActiveWorkContract::Verification {
            commands: requested_work.verification_commands,
            failing_labels: Vec::new(),
            repair_required: false,
            targets: Vec::new(),
        });
    }

    state_native_active_work_contract(state)
}

fn verification_commands_for_active_repair(
    state: &SessionStateSnapshot,
    requested_work: &RequestedWorkDiscipline,
) -> Vec<String> {
    if !state.verification.required_commands.is_empty() {
        state.verification.required_commands.clone()
    } else {
        requested_work.verification_commands.clone()
    }
}

fn verification_repair_targets_from_state(
    state: &SessionStateSnapshot,
) -> Option<Vec<Utf8PathBuf>> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    let failure_cluster = state.verification.failure_cluster.as_ref();
    for target in state
        .verification
        .failure_cluster
        .as_ref()
        .into_iter()
        .flat_map(|cluster| cluster.evidence.iter())
        .filter_map(|evidence| evidence.target.as_deref())
        .chain(
            state
                .failure
                .as_ref()
                .into_iter()
                .flat_map(|failure| failure.targets.iter().map(|target| target.as_str())),
        )
        .chain(state.active_targets.iter().map(|target| target.as_str()))
    {
        let Some(normalized_path) = normalize_target_path(target, Utf8Path::new("")) else {
            continue;
        };
        if !is_code_or_test_target(&normalized_path) && !is_documentation_target(&normalized_path) {
            continue;
        }
        let normalized = normalized_path.as_str().replace('\\', "/");
        if seen.insert(normalized.to_ascii_lowercase()) {
            targets.push(normalized_path);
        }
    }
    if targets.is_empty() {
        return None;
    }
    targets.sort_by_key(|target| {
        (
            target_is_test_like(target.as_str()),
            target.as_str().to_ascii_lowercase(),
        )
    });
    if let Some(cluster) = failure_cluster {
        let generated_test_targets =
            generated_test_exact_repair_targets_from_cluster(cluster, &targets, Utf8Path::new(""));
        if !generated_test_targets.is_empty() {
            return Some(generated_test_targets);
        }
    }
    if verification_cluster_has_source_owned_generated_test_fallback(failure_cluster) {
        let source_targets =
            source_owned_active_work_repair_targets_from_generated_test_evidence(&targets);
        if !source_targets.is_empty() {
            return Some(source_targets);
        }
    }
    Some(targets)
}

fn source_owned_active_work_repair_targets_from_generated_test_evidence(
    targets: &[Utf8PathBuf],
) -> Vec<Utf8PathBuf> {
    let mut source_targets = targets
        .iter()
        .filter(|target| is_code_or_test_target(target) && !is_test_focus_target(target))
        .cloned()
        .collect::<Vec<_>>();
    if source_targets.is_empty() {
        source_targets.extend(
            targets
                .iter()
                .filter(|target| is_test_focus_target(target))
                .filter_map(|target| python_source_for_test_target(target.as_str()))
                .filter_map(|contract| {
                    normalize_target_path(&contract.source_path, Utf8Path::new(""))
                }),
        );
    }
    prioritize_repair_targets(source_targets)
        .into_iter()
        .filter(|target| !is_test_focus_target(target))
        .collect()
}

pub(crate) fn verification_repair_targets_from_state_ignore_diagnostic_scalars_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: widget.compute is missing".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![
            Utf8PathBuf::from("widget.py"),
            Utf8PathBuf::from("test_widget.py"),
        ],
    });
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-diagnostic-scalar-targets".to_string(),
        failing_labels: vec!["test_compute".to_string()],
        primary_failure: Some(
            "AttributeError: module 'widget' has no attribute 'compute'".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("test_compute".to_string()),
            target: Some(" 0".to_string()),
            symbol: Some("widget.compute".to_string()),
            call_site: Some("widget.compute(1 + 2)".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("3".to_string()),
            observed: Some("widget.compute is missing".to_string()),
            public_state_assertions: vec!["widget.compute(1 + 2)".to_string()],
            public_missing_attributes: vec!["widget.compute".to_string()],
            evidence_markers: vec!["public_class_attribute_mismatch".to_string()],
            sibling_obligations: vec!["`widget.compute` is missing".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec![" 0".to_string(), "1 + 2".to_string()],
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["`widget.compute` is missing".to_string()],
        source_refs: vec![" 0".to_string(), "1 + 2".to_string()],
        test_refs: vec!["test_widget.py".to_string()],
    });
    let Some(targets) = verification_repair_targets_from_state(&state) else {
        return false;
    };
    targets == vec![Utf8PathBuf::from("widget.py")]
}

pub(crate) fn public_output_stream_source_repair_active_work_uses_source_target_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "public output source repair target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public stderr assertion mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.verification.failing_labels = vec!["test_public_stderr".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-public-output-source-target".to_string(),
        failing_labels: vec!["test_public_stderr".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("test_public_stderr".to_string()),
            target: Some("expected stderr token".to_string()),
            symbol: None,
            call_site: Some("self.assertIn(\"expected stderr token\", result.stderr)".to_string()),
            exception: None,
            expected: Some("expected stderr token".to_string()),
            observed: Some("stderr `unmatched stderr output`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_output_stream:stderr".to_string(),
                "source_public_behavior_assertion".to_string(),
            ],
            sibling_obligations: vec!["stderr contains expected token".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["stderr contains expected token".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let diagnostic = crate::agent::turn_decision::build_turn_decision_diagnostic(
        &state,
        active.as_ref(),
        &crate::agent::prompt::PromptPolicy::default(),
        &allowed,
        Some("auto".to_string()),
    );

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("widget.py")]
    ) && diagnostic.active_targets == vec![Utf8PathBuf::from("widget.py")]
        && diagnostic
            .active_work_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("`widget.py`"))
        && diagnostic
            .repair_lane
            .as_ref()
            .is_some_and(|lane| lane.required_target.as_deref() == Some("widget.py"))
}

fn target_is_test_like(target: &str) -> bool {
    let name = target
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .to_ascii_lowercase();
    name.starts_with("test_") || name.ends_with("_test.py")
}

pub(crate) fn render_active_work_contract(contract: &ActiveWorkContract) -> String {
    format!("Active work contract:\n{}", contract.summary())
}

pub fn render_model_turn_state(state: &ModelTurnState) -> Option<String> {
    let mut lines = vec![
        format!("Route: {}", task_route_label(state.route)),
        format!("Phase: {}", process_phase_label(state.process_phase)),
    ];
    if let Some(active_todo) = &state.active_todo {
        lines.push(format!("Active focus: {active_todo}"));
    }
    if !state.active_targets.is_empty() {
        let mut targets = state
            .active_targets
            .iter()
            .take(3)
            .map(|value| value.as_str().to_string())
            .collect::<Vec<_>>();
        if state.active_targets.len() > 3 {
            targets.push(format!(
                "and {} more target(s)",
                state.active_targets.len() - 3
            ));
        }
        lines.push(format!("Targets: {}", targets.join(", ")));
    }
    if let Some(summary) = &state.failure_summary {
        lines.push(format!("Failure: {summary}"));
    }
    if let Some(summary) = &state.verification_summary {
        lines.push(format!("Verification: {summary}"));
    }
    if let Some(summary) = &state.completion_summary {
        lines.push(format!("Completion gate: {summary}"));
    }

    if lines.len() == 2
        && state.route == TaskRoute::Code
        && state.process_phase == ProcessPhase::Discover
    {
        return None;
    }

    Some(format!("Current run state:\n{}", lines.join("\n")))
}

#[derive(Debug, Clone)]
struct ToolCallMeta {
    tool_name: ToolName,
    arguments_json: String,
}
#[derive(Debug, Clone)]
struct TypedVerificationFailureEvidence {
    failure: FailureState,
    failing_labels: Vec<String>,
    failure_cluster: Option<VerificationFailureCluster>,
    requirement_refs: Vec<String>,
    required_commands: Vec<String>,
}

fn latest_typed_verification_failure_context(
    session: &SessionRecord,
    history_items: &[HistoryItem],
) -> Option<TypedVerificationFailureEvidence> {
    let latest_user_text = latest_user_text_from_history_items(history_items);
    let protected_targets = latest_user_text
        .as_deref()
        .map(protected_artifact_targets_from_text_as_paths)
        .unwrap_or_default();
    let mut latest_failure = None;
    let mut recent_source_change_targets: Vec<Utf8PathBuf> = Vec::new();
    let mut recent_generated_test_change_targets: Vec<Utf8PathBuf> = Vec::new();

    for item in history_items_in_sequence(history_items) {
        match &item.payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::Message {
                role: MessageRole::User,
                content,
                ..
            } => {
                recent_source_change_targets.clear();
                recent_generated_test_change_targets.clear();
                let text = content_text(content);
                if let Some(evidence) =
                    typed_verification_failure_from_continuation_text(session.cwd.as_path(), &text)
                {
                    latest_failure = Some(evidence);
                }
            }
            HistoryItemPayload::FileChange { changes, .. } => {
                let changed_targets = file_change_repair_targets(changes, &session.cwd)
                    .into_iter()
                    .filter(|target| !is_scenario_contract_ref(target.as_str()))
                    .collect::<Vec<_>>();
                let source_targets = changed_targets
                    .iter()
                    .cloned()
                    .filter(|target| !is_test_focus_target(target))
                    .collect::<Vec<_>>();
                if !source_targets.is_empty() {
                    recent_source_change_targets = source_targets;
                }
                let generated_test_targets = changed_targets
                    .into_iter()
                    .filter(|target| is_test_focus_target(target))
                    .collect::<Vec<_>>();
                if !generated_test_targets.is_empty() {
                    recent_generated_test_change_targets = generated_test_targets;
                }
            }
            HistoryItemPayload::ToolOutput {
                status,
                verification_run: Some(run),
                ..
            } if *status == crate::protocol::ToolLifecycleStatus::Completed => match run.status {
                VerificationRunStatus::Passed => {
                    latest_failure = None;
                    recent_source_change_targets.clear();
                    recent_generated_test_change_targets.clear();
                }
                VerificationRunStatus::Failed | VerificationRunStatus::TimedOut => {
                    let mut failure_cluster = run.failure_cluster.clone();
                    enrich_generated_test_local_binding_contradiction_cluster(
                        failure_cluster.as_mut(),
                        &run.output_summary,
                        &session.cwd,
                    );
                    let summary_targets =
                        extract_failure_paths_from_text(&run.output_summary, &session.cwd);
                    let command_targets =
                        extract_verification_scope_targets(Some(&run.command), &session.cwd);
                    let source_refs = failure_cluster
                        .as_ref()
                        .into_iter()
                        .flat_map(|cluster| {
                            cluster.source_refs.iter().chain(cluster.test_refs.iter())
                        })
                        .filter_map(|target| normalize_target_path(target, &session.cwd))
                        .collect::<Vec<_>>();
                    let mut targets = source_refs;
                    targets.extend(summary_targets);
                    targets.extend(command_targets);
                    if verification_cluster_has_no_tests_ran(failure_cluster.as_ref()) {
                        targets.extend(recent_generated_test_change_targets.iter().cloned());
                    }
                    targets = merge_recent_source_targets_for_source_owned_failure(
                        targets,
                        &recent_source_change_targets,
                        failure_cluster.as_ref(),
                        matches!(run.status, VerificationRunStatus::TimedOut),
                    );
                    let targets = filter_verification_repair_targets(
                        prioritize_repair_targets(filter_protected_reference_targets(
                            targets,
                            &protected_targets,
                        )),
                        session.cwd.as_path(),
                    );
                    let summary = enrich_verification_failure_summary_with_test_requirement_context(
                        &compact_verification_failure_summary(
                            Some(&run.command),
                            "typed verification run",
                            &run.output_summary,
                        ),
                        &run.output_summary,
                        &session.cwd,
                    );
                    let failing_labels = run
                        .failure_cluster
                        .as_ref()
                        .map(|cluster| cluster.failing_labels.clone())
                        .unwrap_or_default();
                    latest_failure = Some(TypedVerificationFailureEvidence {
                        failure: FailureState {
                            kind: FailureKind::VerificationFailed,
                            summary,
                            tool_name: Some(ToolName::Shell),
                            targets,
                        },
                        failing_labels,
                        failure_cluster,
                        requirement_refs: run.requirement_refs.clone(),
                        required_commands: vec![run.command.clone()],
                    });
                }
                VerificationRunStatus::NotVerification => {}
            },
            _ => {}
        }
    }

    latest_failure
}

pub(crate) fn message_user_protected_reference_filters_verification_targets_fixture_passes() -> bool
{
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "message user protected target fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let history_items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Repair widget.py according to scenario_contract.md, but do not change scenario_contract.md.".to_string(),
                }],
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Verification failed".to_string(),
                output_text: "public behavior mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-message-user-protected-target".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: public behavior mismatch".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-message-user-protected-target".to_string(),
                        failing_labels: vec!["test_public_behavior".to_string()],
                        primary_failure: Some("public behavior mismatch".to_string()),
                        evidence: vec![VerificationFailureEvidence {
                            evidence_kind: "verification_failure".to_string(),
                            subtype: Some("public_state_assertion_mismatch".to_string()),
                            label: Some("test_public_behavior".to_string()),
                            target: Some("widget.py".to_string()),
                            symbol: None,
                            call_site: Some("widget.run()".to_string()),
                            exception: None,
                            expected: Some("contract behavior".to_string()),
                            observed: Some("wrong behavior".to_string()),
                            public_state_assertions: vec!["widget.run()".to_string()],
                            public_missing_attributes: Vec::new(),
                            evidence_markers: vec!["source_public_behavior_assertion".to_string()],
                            sibling_obligations: Vec::new(),
                            requirement_refs: Vec::new(),
                            source_refs: vec![
                                "widget.py".to_string(),
                                "scenario_contract.md".to_string(),
                            ],
                            test_refs: vec!["test_widget.py".to_string()],
                        }],
                        sibling_obligations: Vec::new(),
                        source_refs: vec![
                            "widget.py".to_string(),
                            "scenario_contract.md".to_string(),
                        ],
                        test_refs: vec!["test_widget.py".to_string()],
                    }),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];

    let Some(evidence) = latest_typed_verification_failure_context(&session, &history_items) else {
        return false;
    };
    evidence
        .failure
        .targets
        .iter()
        .any(|target| target.as_str() == "widget.py")
        && evidence
            .failure
            .targets
            .iter()
            .all(|target| target.as_str() != "scenario_contract.md")
        && evidence.required_commands == vec!["python -m unittest".to_string()]
}

fn verification_cluster_has_no_tests_ran(cluster: Option<&VerificationFailureCluster>) -> bool {
    cluster.is_some_and(|cluster| {
        cluster.evidence.iter().any(|evidence| {
            evidence.subtype.as_deref() == Some("no_tests_ran")
                || evidence
                    .evidence_markers
                    .iter()
                    .any(|marker| marker == "no_tests_ran")
        })
    })
}

fn typed_verification_failure_from_continuation_text(
    workspace_root: &Utf8Path,
    text: &str,
) -> Option<TypedVerificationFailureEvidence> {
    if !looks_like_verification_repair_continuation_text(text) {
        return None;
    }

    let repair_targets = section_list_items(
        text,
        &["repair targets", "active repair targets", "repair target"],
    )
    .into_iter()
    .filter_map(|target| normalize_target_path(&strip_inline_code_ticks(&target), workspace_root))
    .filter(|target| {
        is_code_or_test_target(target)
            || is_documentation_target(target)
            || workspace_root.join(target.as_str()).exists()
    })
    .collect::<Vec<_>>();
    if repair_targets.is_empty() {
        return None;
    }

    let mut required_commands = section_list_items(
        text,
        &[
            "failed required verification commands",
            "required verification failed in the latest evidence",
            "required verification commands",
        ],
    )
    .into_iter()
    .map(|command| strip_inline_code_ticks(&command))
    .filter(|command| !command.trim().is_empty())
    .collect::<Vec<_>>();
    if required_commands.is_empty() {
        required_commands = explicit_verification_commands_from_text(text);
    }
    required_commands = dedupe_string_values(required_commands);

    let evidence_lines = section_list_items(
        text,
        &[
            "latest verification failure evidence",
            "verification failure evidence",
            "latest failure evidence",
            "failure evidence",
        ],
    );
    let evidence_summary = if evidence_lines.is_empty() {
        required_commands
            .first()
            .map(|command| format!("failed required verification command: {command}"))
            .unwrap_or_else(|| "failed required verification command".to_string())
    } else {
        evidence_lines
            .iter()
            .map(|line| strip_inline_code_ticks(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let primary_target = repair_targets
        .first()
        .map(|target| target.as_str().to_string());
    let summary = compact_public_command_contract_continuation_summary(
        &evidence_summary,
        &required_commands,
        primary_target.as_deref(),
    )
    .unwrap_or_else(|| {
        compact_verification_failure_summary(
            required_commands.first().map(String::as_str),
            "verification-repair continuation",
            &evidence_summary,
        )
    });
    let failing_labels =
        verification_repair_continuation_failure_labels(&evidence_lines, &required_commands);
    let cluster_evidence = continuation_verification_failure_evidence(
        workspace_root,
        &evidence_summary,
        primary_target.as_deref(),
        required_commands.first().map(String::as_str),
    );

    Some(TypedVerificationFailureEvidence {
        failure: FailureState {
            kind: FailureKind::VerificationFailed,
            summary: summary.clone(),
            tool_name: Some(ToolName::Shell),
            targets: repair_targets,
        },
        failing_labels: failing_labels.clone(),
        failure_cluster: Some(VerificationFailureCluster {
            cluster_id: "stop-hook-verification-repair-continuation".to_string(),
            failing_labels,
            primary_failure: Some(summary),
            evidence: cluster_evidence,
            sibling_obligations: Vec::new(),
            source_refs: Vec::new(),
            test_refs: Vec::new(),
        }),
        requirement_refs: Vec::new(),
        required_commands,
    })
}

fn compact_public_command_contract_continuation_summary(
    evidence_summary: &str,
    required_commands: &[String],
    primary_target: Option<&str>,
) -> Option<String> {
    let lower = evidence_summary.to_ascii_lowercase();
    if !(lower.contains("public_command_contract")
        || lower.contains("public command contract")
        || lower.contains("route-owned public argv command contract"))
    {
        return None;
    }
    let mut parts = Vec::new();
    parts.push("public_command_contract_failed".to_string());
    if let Some(target) = primary_target {
        parts.push(format!("target={target}"));
    }
    if !required_commands.is_empty() {
        parts.push(format!("failed_commands={}", required_commands.len()));
    }
    if lower.contains("interactive stdin") || lower.contains("eoferror") {
        parts.push("observed=argv invocation entered interactive stdin mode instead of processing command-line arguments".to_string());
    } else if lower.contains("stdout had no line ending") {
        parts.push("observed=stdout result suffix missing".to_string());
    } else if lower.contains("stderr contained none") || lower.contains("stdout contained none") {
        parts.push("observed=usage/help/error output observation missing".to_string());
    }
    parts.push(
        "expected=direct argv command handling preserves route-owned exit/stdout/stderr contract"
            .to_string(),
    );
    Some(parts.join("; "))
}

fn continuation_verification_failure_evidence(
    workspace_root: &Utf8Path,
    evidence_summary: &str,
    primary_target: Option<&str>,
    required_command: Option<&str>,
) -> Vec<VerificationFailureEvidence> {
    let mut evidence = crate::agent::repair_lane::verification_failure_evidence_from_summary(
        FailureKind::VerificationFailed,
        evidence_summary,
    );
    if evidence.is_empty() {
        evidence.push(VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("stop_hook_verification_failure".to_string()),
            label: Some("verification_repair_continuation".to_string()),
            target: primary_target.map(str::to_string),
            symbol: None,
            call_site: required_command.map(str::to_string),
            exception: None,
            expected: Some("required verification command passes after repair".to_string()),
            observed: Some(evidence_summary.to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: Vec::new(),
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: Vec::new(),
        });
    }

    for item in &mut evidence {
        item.label
            .get_or_insert_with(|| "verification_repair_continuation".to_string());
        item.call_site
            .get_or_insert_with(|| required_command.unwrap_or_default().to_string());
        item.expected
            .get_or_insert_with(|| "required verification command passes after repair".to_string());
        item.observed
            .get_or_insert_with(|| evidence_summary.to_string());
        item.evidence_markers
            .push("verification_repair_continuation".to_string());
        item.evidence_markers
            .push("stop_hook_verification_failure".to_string());
        item.evidence_markers.sort();
        item.evidence_markers.dedup();
        if let Some(target) = item
            .target
            .as_deref()
            .and_then(|target| normalize_target_path(target, workspace_root))
        {
            item.target = Some(target.as_str().to_string());
        } else if item.target.is_none() {
            item.target = primary_target.map(str::to_string);
        }
        item.source_refs = item
            .source_refs
            .iter()
            .filter_map(|target| normalize_target_path(target, workspace_root))
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        item.test_refs = item
            .test_refs
            .iter()
            .filter_map(|target| normalize_target_path(target, workspace_root))
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        item.source_refs = dedupe_string_values(std::mem::take(&mut item.source_refs));
        item.test_refs = dedupe_string_values(std::mem::take(&mut item.test_refs));
    }
    evidence
}

fn looks_like_verification_repair_continuation_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_repair_targets = section_has_header(text, &["repair targets", "repair target"]);
    let has_failed_commands = section_has_header(
        text,
        &[
            "failed required verification commands",
            "required verification failed in the latest evidence",
            "required verification commands",
        ],
    );
    has_repair_targets
        && (has_failed_commands
            || lower.contains("verification-repair continuation")
            || lower.contains("latest required verification command failed"))
        && (lower.contains("repair")
            || lower.contains("failed required verification")
            || lower.contains("rerun the failed required verification"))
}

fn verification_repair_continuation_failure_labels(
    evidence_lines: &[String],
    required_commands: &[String],
) -> Vec<String> {
    let mut labels = evidence_lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (key, value) = trimmed.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("label")
                .then(|| strip_inline_code_ticks(value.trim()))
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if labels.is_empty() {
        labels.extend(
            required_commands
                .iter()
                .take(4)
                .map(|command| format!("failed command: {command}")),
        );
    }
    if labels.is_empty() {
        labels.push("verification_repair_continuation".to_string());
    }
    dedupe_string_values(labels)
}

fn dedupe_string_values(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        let key = value.trim().to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(value);
    }
    deduped
}

fn section_has_header(text: &str, headers: &[&str]) -> bool {
    text.lines()
        .map(normalized_state_section_header)
        .any(|header| headers.iter().any(|expected| header == *expected))
}

fn section_list_items(text: &str, headers: &[&str]) -> Vec<String> {
    let known_headers = [
        "repair targets",
        "repair target",
        "active repair targets",
        "failed required verification commands",
        "required verification failed in the latest evidence",
        "required verification commands",
        "latest verification failure evidence",
        "verification failure evidence",
        "latest failure evidence",
        "failure evidence",
        "expected artifacts",
        "missing expected artifacts",
        "open obligations",
        "required verification still missing",
        "case",
        "stage",
        "verification-repair attempt",
        "verification attempt",
        "previous final assistant message",
        "previous assistant message",
    ];
    let mut active = false;
    let mut items = Vec::new();
    for raw_line in text.lines() {
        let normalized = normalized_state_section_header(raw_line);
        if headers.iter().any(|header| normalized == *header) {
            active = true;
            continue;
        }
        if active && known_headers.iter().any(|header| normalized == *header) {
            active = false;
            continue;
        }
        if !active {
            continue;
        }
        let item = raw_line
            .trim()
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim();
        if !item.is_empty() {
            items.push(item.to_string());
        }
    }
    items
}

fn normalized_state_section_header(line: &str) -> String {
    line.trim()
        .trim_start_matches("- ")
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
}

fn strip_inline_code_ticks(value: &str) -> String {
    value.trim().trim_matches('`').trim().to_string()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GeneratedTestLocalBindingContradiction {
    test_target: Utf8PathBuf,
    label: String,
    identifier: String,
    assignment_line: String,
    assertion_line: String,
}

fn enrich_generated_test_local_binding_contradiction_cluster(
    cluster: Option<&mut VerificationFailureCluster>,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) {
    let Some(cluster) = cluster else {
        return;
    };
    let contradictions =
        generated_test_local_binding_contradictions(cluster, raw_summary, workspace_root);
    if contradictions.is_empty() {
        return;
    }

    for contradiction in contradictions {
        let target = contradiction.test_target.as_str().to_string();
        let marker = "generated_test_local_binding_contradiction".to_string();
        let readable_marker = format!(
            "generated test local binding contradiction `{}`",
            contradiction.identifier
        );
        let context_marker = format!(
            "generated test local binding contradiction: {} -> {}",
            contradiction.assignment_line, contradiction.assertion_line
        );
        if !cluster.test_refs.iter().any(|existing| existing == &target) {
            cluster.test_refs.push(target.clone());
        }
        for sibling in [&marker, &readable_marker] {
            if !cluster
                .sibling_obligations
                .iter()
                .any(|existing| existing == sibling)
            {
                cluster.sibling_obligations.push(sibling.clone());
            }
        }

        let mut enriched = false;
        for evidence in &mut cluster.evidence {
            let evidence_points_to_target = evidence
                .target
                .as_deref()
                .is_some_and(|existing| file_name_str(existing) == file_name_str(&target))
                || evidence
                    .test_refs
                    .iter()
                    .any(|existing| file_name_str(existing) == file_name_str(&target));
            if !evidence_points_to_target {
                continue;
            }
            if evidence.target.is_none() {
                evidence.target = Some(target.clone());
            }
            if evidence.call_site.is_none() {
                evidence.call_site = Some(contradiction.assignment_line.clone());
            }
            if evidence.observed.is_none() {
                evidence.observed = Some(format!(
                    "local `{}` overwritten by duplicate destructuring before assertion",
                    contradiction.identifier
                ));
            }
            if !evidence
                .test_refs
                .iter()
                .any(|existing| existing == &target)
            {
                evidence.test_refs.push(target.clone());
            }
            push_unique_string(&mut evidence.evidence_markers, marker.clone());
            push_unique_string(&mut evidence.evidence_markers, readable_marker.clone());
            push_unique_string(&mut evidence.evidence_markers, context_marker.clone());
            push_unique_string(&mut evidence.sibling_obligations, marker.clone());
            push_unique_string(&mut evidence.sibling_obligations, readable_marker.clone());
            enriched = true;
        }
        if !enriched {
            cluster.evidence.push(VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("public_state_assertion_mismatch".to_string()),
                label: Some(contradiction.label.clone()),
                target: Some(target.clone()),
                symbol: None,
                call_site: Some(contradiction.assignment_line.clone()),
                exception: None,
                expected: None,
                observed: Some(format!(
                    "local `{}` overwritten by duplicate destructuring before assertion",
                    contradiction.identifier
                )),
                public_state_assertions: vec![contradiction.identifier.clone()],
                public_missing_attributes: Vec::new(),
                evidence_markers: vec![marker.clone(), readable_marker.clone(), context_marker],
                sibling_obligations: vec![marker, readable_marker],
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec![target],
            });
        }
    }
    cluster.test_refs.sort();
    cluster.test_refs.dedup();
    cluster.sibling_obligations.sort();
    cluster.sibling_obligations.dedup();
}

fn generated_test_local_binding_contradictions(
    cluster: &VerificationFailureCluster,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<GeneratedTestLocalBindingContradiction> {
    let labels = if cluster.failing_labels.is_empty() {
        extract_verification_failure_labels(raw_summary)
    } else {
        cluster.failing_labels.clone()
    };
    if labels.is_empty() {
        return Vec::new();
    }
    let assertion_subjects = local_unittest_assertion_subjects(raw_summary);
    if assertion_subjects.is_empty() {
        return Vec::new();
    }

    let mut test_targets = cluster
        .test_refs
        .iter()
        .filter_map(|target| normalize_target_path(target, workspace_root))
        .collect::<Vec<_>>();
    test_targets.extend(
        extract_failure_paths_from_text(raw_summary, workspace_root)
            .into_iter()
            .filter(|target| is_test_focus_target(target)),
    );
    test_targets = prioritize_repair_targets(test_targets);

    let mut contradictions = Vec::new();
    for target in test_targets {
        let Some(source) = read_small_test_context_source(&target, workspace_root) else {
            continue;
        };
        for label in &labels {
            if let Some(contradiction) = generated_test_local_binding_contradiction_for_label(
                &target,
                label,
                &source,
                &assertion_subjects,
            ) {
                contradictions.push(contradiction);
            }
        }
    }
    contradictions
}

fn generated_test_local_binding_contradiction_for_label(
    target: &Utf8PathBuf,
    label: &str,
    source: &str,
    assertion_subjects: &[String],
) -> Option<GeneratedTestLocalBindingContradiction> {
    let lines = source.lines().collect::<Vec<_>>();
    let method_index = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    })?;
    let method_indent = leading_space_count(lines[method_index]);
    let method_end = lines[method_index + 1..]
        .iter()
        .position(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty()
                && leading_space_count(line) <= method_indent
                && (trimmed.starts_with("def ") || trimmed.starts_with("class "))
        })
        .map(|offset| method_index + 1 + offset)
        .unwrap_or(lines.len());
    let body = &lines[method_index + 1..method_end];
    for (assertion_index, assertion_line) in body.iter().enumerate() {
        let assertion_line = assertion_line.trim();
        if !assertion_line.contains("self.assert") {
            continue;
        }
        let Some(asserted_subject) = assertion_subjects
            .iter()
            .find(|subject| assertion_line_contains_identifier(assertion_line, subject))
        else {
            continue;
        };
        for assignment_line in body[..assertion_index].iter().rev() {
            let assignment_line = assignment_line.trim();
            let duplicates = duplicate_destructuring_identifiers(assignment_line);
            if duplicates.iter().any(|item| item == asserted_subject) {
                return Some(GeneratedTestLocalBindingContradiction {
                    test_target: target.clone(),
                    label: label.to_string(),
                    identifier: asserted_subject.clone(),
                    assignment_line: assignment_line.to_string(),
                    assertion_line: assertion_line.to_string(),
                });
            }
        }
    }
    None
}

fn local_unittest_assertion_subjects(summary: &str) -> Vec<String> {
    let mut subjects = local_boolean_assertion_subjects(summary);
    for line in failure_summary_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(assert_start) = trimmed.find("self.assert") else {
            continue;
        };
        let rest = &trimmed[assert_start..];
        let Some(open_index) = rest.find('(') else {
            continue;
        };
        let after_open = &rest[open_index + 1..];
        let end = after_open
            .find(',')
            .or_else(|| after_open.find(')'))
            .unwrap_or(after_open.len());
        let subject = after_open[..end].trim();
        if is_local_identifier(subject) && !subjects.iter().any(|existing| existing == subject) {
            subjects.push(subject.to_string());
        }
    }
    subjects.sort();
    subjects.dedup();
    subjects
}

fn duplicate_destructuring_identifiers(line: &str) -> Vec<String> {
    if line.contains("==") || line.contains("!=") || line.contains("<=") || line.contains(">=") {
        return Vec::new();
    }
    let Some((lhs, _)) = line.split_once('=') else {
        return Vec::new();
    };
    if !lhs.contains(',') {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for raw in lhs
        .trim()
        .trim_matches(|ch| matches!(ch, '(' | ')' | '[' | ']'))
        .split(',')
    {
        let identifier = raw.trim();
        if identifier == "_" || !is_local_identifier(identifier) {
            continue;
        }
        if !seen.insert(identifier.to_string()) {
            duplicates.insert(identifier.to_string());
        }
    }
    duplicates.into_iter().collect()
}

fn assertion_line_contains_identifier(line: &str, identifier: &str) -> bool {
    line.split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .any(|token| token == identifier)
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn file_name_str(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn repair_progress_authority_targets(
    evidence: &TypedVerificationFailureEvidence,
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    if let Some(cluster) = evidence.failure_cluster.as_ref() {
        let generated_test_targets = generated_test_exact_repair_targets_from_cluster(
            cluster,
            &evidence.failure.targets,
            workspace_root,
        );
        if !generated_test_targets.is_empty() {
            return generated_test_targets;
        }
    }
    let mut targets = evidence.failure.targets.clone();
    if verification_cluster_has_source_owned_generated_test_fallback(
        evidence.failure_cluster.as_ref(),
    ) {
        let test_targets = targets
            .iter()
            .cloned()
            .chain(
                evidence
                    .failure_cluster
                    .as_ref()
                    .into_iter()
                    .flat_map(|cluster| {
                        cluster.test_refs.iter().chain(
                            cluster
                                .evidence
                                .iter()
                                .flat_map(|item| item.test_refs.iter()),
                        )
                    })
                    .filter_map(|target| normalize_target_path(target, workspace_root)),
            )
            .filter(|target| is_test_focus_target(target))
            .collect::<Vec<_>>();
        for target in test_targets {
            if let Some(contract) = python_source_for_test_target(target.as_str()) {
                if let Some(source) = normalize_target_path(&contract.source_path, workspace_root) {
                    targets.push(source);
                }
            }
        }
    }
    prioritize_repair_targets(targets)
}

fn source_owned_repair_targets_include_test_evidence_and_source(targets: &[Utf8PathBuf]) -> bool {
    let has_test_evidence = targets.iter().any(|target| is_test_focus_target(target));
    let has_source_authority = targets
        .iter()
        .any(|target| is_code_or_test_target(target) && !is_test_focus_target(target));
    has_test_evidence && has_source_authority
}

fn verification_cluster_has_source_owned_generated_test_fallback(
    cluster: Option<&VerificationFailureCluster>,
) -> bool {
    verification_cluster_prefers_source_repair(cluster)
        || verification_cluster_has_source_owned_public_behavior_evidence(cluster)
        || cluster.is_some_and(|cluster| {
            cluster.evidence.iter().any(|evidence| {
                evidence.subtype.as_deref() == Some("generic_verification_failure")
                    && (!evidence.public_state_assertions.is_empty()
                        || !evidence.sibling_obligations.is_empty()
                        || evidence
                            .call_site
                            .as_deref()
                            .is_some_and(|call_site| call_site.contains(".py")))
                    && evidence
                        .test_refs
                        .iter()
                        .chain(cluster.test_refs.iter())
                        .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
                        .any(|target| is_test_focus_target(&target))
            })
        })
}

fn verification_cluster_has_source_owned_public_behavior_evidence(
    cluster: Option<&VerificationFailureCluster>,
) -> bool {
    let Some(cluster) = cluster else {
        return false;
    };
    let has_source_public_behavior_failure = cluster.evidence.iter().any(|evidence| {
        matches!(
            evidence.subtype.as_deref(),
            Some("generic_verification_failure")
                | Some("public_output_stream_assertion_mismatch")
                | Some("public_state_assertion_mismatch")
                | Some("public_class_attribute_mismatch")
        ) || !evidence.public_missing_attributes.is_empty()
    });
    if !has_source_public_behavior_failure {
        return false;
    }
    let has_generated_test_refs = cluster
        .test_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.test_refs.iter()),
        )
        .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
        .any(|target| is_test_focus_target(&target));
    if !has_generated_test_refs {
        return false;
    }
    if verification_cluster_has_source_public_callable_obligation(cluster) {
        return true;
    }
    if verification_cluster_has_generated_test_ownership_markers(cluster) {
        return false;
    }
    if cluster.evidence.iter().any(|evidence| {
        evidence.subtype.as_deref() == Some("public_output_stream_assertion_mismatch")
            && evidence
                .evidence_markers
                .iter()
                .chain(evidence.sibling_obligations.iter())
                .chain(cluster.sibling_obligations.iter())
                .any(|marker| marker == "source_public_behavior_assertion")
    }) {
        return true;
    }
    cluster
        .source_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.source_refs.iter()),
        )
        .any(|source_ref| {
            let trimmed = source_ref.trim();
            !trimmed.is_empty()
                && normalize_target_path(trimmed, Utf8Path::new(""))
                    .is_none_or(|target| !is_code_or_test_target(&target))
        })
}

fn verification_cluster_has_generated_test_ownership_markers(
    cluster: &VerificationFailureCluster,
) -> bool {
    if verification_cluster_has_source_public_callable_obligation(cluster) {
        return false;
    }
    cluster.evidence.iter().any(|evidence| {
        evidence
            .evidence_markers
            .iter()
            .chain(evidence.requirement_refs.iter())
            .chain(cluster.sibling_obligations.iter())
            .any(|marker| {
                let marker = marker.to_ascii_lowercase();
                marker.contains("generated-test data model contradicts")
                    || marker.contains("generated test setup contradicts")
                    || marker.contains("generated-test setup contradicts")
                    || marker.contains("generated test artifact name resolution defect")
                    || marker.contains("generated_test_artifact_name_resolution_defect")
                    || marker.contains("generated_test_artifact_api_misuse")
                    || marker.contains("generated test invalid reflection subject")
                    || marker.contains("generated_test_subprocess_encoding_missing")
                    || marker.contains("generated test subprocess child encoding missing")
                    || marker.contains("generated_test_subprocess_output_capture_missing")
                    || marker.contains("generated test subprocess output capture missing")
                    || marker.contains("generated test artifact parse defect")
                    || marker.contains("generated_test_artifact_parse_defect")
                    || marker.contains("generated-test contract overreach")
                    || marker.contains("generated_test_contract_overreach")
                    || marker.contains("generated-test contract")
                    || marker.contains("generated-test conflict evidence")
                    || marker.contains("generated_test_local_binding_contradiction")
                    || marker.contains("generated test local binding contradiction")
                    || marker.contains("generated-test local binding contradiction")
                    || marker.contains("generated-test logging side-effect assertion")
                    || marker.contains("generated_test_out_of_scope")
                    || marker.contains("testviolatescontract")
            })
    })
}

fn generated_test_exact_repair_targets_from_cluster(
    cluster: &VerificationFailureCluster,
    fallback_targets: &[Utf8PathBuf],
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    if !verification_cluster_has_generated_test_exact_repair_markers(cluster) {
        return Vec::new();
    }
    let mut targets = Vec::new();
    targets.extend(
        cluster
            .test_refs
            .iter()
            .filter_map(|target| normalize_target_path(target, workspace_root)),
    );
    for evidence in &cluster.evidence {
        targets.extend(
            evidence
                .test_refs
                .iter()
                .filter_map(|target| normalize_target_path(target, workspace_root)),
        );
        if let Some(target) = evidence.target.as_deref() {
            if let Some(target) = normalize_target_path(target, workspace_root) {
                if is_test_focus_target(&target) {
                    targets.push(target);
                }
            }
        }
    }
    targets.extend(
        fallback_targets
            .iter()
            .filter(|target| is_test_focus_target(target))
            .cloned(),
    );
    prioritize_repair_targets(targets)
        .into_iter()
        .filter(|target| is_test_focus_target(target))
        .collect()
}

fn verification_cluster_has_generated_test_exact_repair_markers(
    cluster: &VerificationFailureCluster,
) -> bool {
    if verification_cluster_has_source_public_callable_obligation(cluster) {
        return false;
    }
    cluster.evidence.iter().any(|evidence| {
        let points_to_generated_test = evidence
            .test_refs
            .iter()
            .chain(cluster.test_refs.iter())
            .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
            .any(|target| is_test_focus_target(&target))
            || evidence
                .target
                .as_deref()
                .and_then(|target| normalize_target_path(target, Utf8Path::new("")))
                .is_some_and(|target| is_test_focus_target(&target));
        points_to_generated_test
            && (evidence.subtype.as_deref() == Some("generated_test_logging_contract_overreach")
                || generated_test_parse_defect_evidence_requires_exact_test_repair(
                    evidence, cluster,
                )
                || evidence
                    .evidence_markers
                    .iter()
                    .chain(evidence.requirement_refs.iter())
                    .chain(evidence.sibling_obligations.iter())
                    .chain(cluster.sibling_obligations.iter())
                    .any(|marker| {
                        let marker = marker.to_ascii_lowercase();
                        marker.contains("generated_test_artifact_name_resolution_defect")
                            || marker.contains("generated test artifact name resolution defect")
                            || marker.contains("generated test name-resolution")
                            || marker.contains("generated test missing name")
                            || marker.contains("generated_test_artifact_api_misuse")
                            || marker.contains("generated test invalid reflection subject")
                            || marker.contains("generated_test_subprocess_encoding_missing")
                            || marker.contains("generated test subprocess child encoding missing")
                            || marker.contains("generated_test_subprocess_output_capture_missing")
                            || marker.contains("generated test subprocess output capture missing")
                            || marker.contains("generated_test_artifact_parse_defect")
                            || marker.contains("generated test artifact parse defect")
                            || marker.contains("generated_test_contract_overreach")
                            || marker.contains("generated-test contract overreach")
                            || marker.contains("generated_test_local_binding_contradiction")
                            || marker.contains("generated test local binding contradiction")
                            || marker.contains("generated-test local binding contradiction")
                            || marker.contains("generated-test logging side-effect assertion")
                            || marker.contains("generated_test_out_of_scope")
                            || marker.contains("testviolatescontract")
                    }))
    })
}

fn verification_cluster_has_source_public_callable_obligation(
    cluster: &VerificationFailureCluster,
) -> bool {
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

fn generated_test_parse_defect_evidence_requires_exact_test_repair(
    evidence: &VerificationFailureEvidence,
    cluster: &VerificationFailureCluster,
) -> bool {
    let is_parse_defect = evidence.subtype.as_deref() == Some("source_parse_defect")
        || evidence
            .evidence_markers
            .iter()
            .any(|marker| marker == "source_parse_defect");
    if !is_parse_defect {
        return false;
    }
    if evidence
        .target
        .as_deref()
        .and_then(|target| normalize_target_path(target, Utf8Path::new("")))
        .is_some_and(|target| is_test_focus_target(&target))
    {
        return true;
    }
    if verification_cluster_has_mutable_source_target(cluster) {
        return false;
    }
    evidence
        .test_refs
        .iter()
        .chain(cluster.test_refs.iter())
        .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
        .any(|target| is_test_focus_target(&target))
}

fn verification_cluster_has_mutable_source_target(cluster: &VerificationFailureCluster) -> bool {
    cluster
        .source_refs
        .iter()
        .chain(
            cluster
                .evidence
                .iter()
                .flat_map(|evidence| evidence.source_refs.iter()),
        )
        .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
        .any(|target| is_code_or_test_target(&target) && !is_test_focus_target(&target))
        || cluster.evidence.iter().any(|evidence| {
            evidence
                .target
                .as_deref()
                .and_then(|target| normalize_target_path(target, Utf8Path::new("")))
                .is_some_and(|target| {
                    is_code_or_test_target(&target) && !is_test_focus_target(&target)
                })
        })
}

fn file_change_repair_targets(
    changes: &[FileChangeEvidence],
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let targets = changes
        .iter()
        .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
        .filter_map(|path| normalize_target_path(path.as_str(), workspace_root))
        .filter(|target| is_code_or_test_target(target))
        .collect::<Vec<_>>();
    prioritize_repair_targets(targets)
}

fn merge_recent_source_targets_for_source_owned_failure(
    mut targets: Vec<Utf8PathBuf>,
    recent_change_targets: &[Utf8PathBuf],
    cluster: Option<&VerificationFailureCluster>,
    timed_out: bool,
) -> Vec<Utf8PathBuf> {
    if !timed_out && !verification_cluster_prefers_source_repair(cluster) {
        return targets;
    }
    let recent_source_targets = recent_change_targets
        .iter()
        .filter(|target| is_code_or_test_target(target) && !is_test_focus_target(target))
        .cloned()
        .collect::<Vec<_>>();
    if recent_source_targets.is_empty() {
        return targets;
    }
    let has_explicit_source_ref = verification_cluster_has_source_refs(cluster);
    let target_has_source = targets
        .iter()
        .any(|target| is_code_or_test_target(target) && !is_test_focus_target(target));
    if has_explicit_source_ref && target_has_source {
        return targets;
    }
    let mut merged = recent_source_targets;
    merged.append(&mut targets);
    prioritize_repair_targets(merged)
}

fn verification_cluster_has_source_refs(cluster: Option<&VerificationFailureCluster>) -> bool {
    cluster.is_some_and(|cluster| {
        cluster
            .source_refs
            .iter()
            .chain(
                cluster
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.source_refs.iter()),
            )
            .filter_map(|target| normalize_target_path(target, Utf8Path::new("")))
            .any(|target| is_code_or_test_target(&target) && !is_test_focus_target(&target))
    })
}

fn verification_cluster_prefers_source_repair(
    cluster: Option<&VerificationFailureCluster>,
) -> bool {
    cluster.is_some_and(|cluster| {
        if verification_cluster_has_generated_test_exact_repair_markers(cluster) {
            return false;
        }
        cluster.evidence.iter().any(|evidence| {
            matches!(
                evidence.subtype.as_deref(),
                Some(
                    "public_state_assertion_mismatch"
                        | "public_class_attribute_mismatch"
                        | "public_constructor_body_exception"
                        | "public_constructor_signature_mismatch"
                        | "public_callable_signature_mismatch"
                        | "public_exception_mismatch"
                        | "public_method_attribute_mismatch"
                        | "public_missing_attribute_mismatch"
                        | "source_import_time_name_resolution"
                        | "source_parse_defect"
                )
            )
        })
    })
}

fn filter_verification_repair_targets(
    targets: Vec<Utf8PathBuf>,
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let file_authoritative = targets
        .iter()
        .filter(|target| {
            is_code_or_test_target(target)
                || workspace_root.join(target).is_file()
                || workspace_root.join(target).is_dir()
        })
        .cloned()
        .collect::<Vec<_>>();
    if file_authoritative.is_empty() {
        targets
    } else {
        prioritize_repair_targets(file_authoritative)
    }
}

fn verification_failure_repair_targets(
    current_targets: Vec<Utf8PathBuf>,
    failure_targets: Vec<Utf8PathBuf>,
) -> Vec<Utf8PathBuf> {
    let current_file_authoritative = current_targets
        .iter()
        .filter(|target| is_code_or_test_target(target) || is_documentation_target(target))
        .cloned()
        .collect::<Vec<_>>();
    let file_authoritative = failure_targets
        .iter()
        .filter(|target| is_code_or_test_target(target))
        .cloned()
        .collect::<Vec<_>>();
    let mut merged = if current_file_authoritative.is_empty() && file_authoritative.is_empty() {
        current_targets
    } else {
        current_file_authoritative
    };
    if file_authoritative.is_empty() {
        merged.extend(failure_targets);
    } else {
        merged.extend(file_authoritative);
    }
    prioritize_repair_targets(merged)
}

fn contract_reconciled_verification_repair_targets(
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
) -> Option<Vec<Utf8PathBuf>> {
    let decision =
        crate::agent::contract_reconciliation::reconcile_session_state_failure_with_cluster(
            state,
            state.verification.failure_cluster.as_ref(),
        )?;
    if decision.fail_closed() || !(decision.source_repair_allowed || decision.test_repair_allowed) {
        return None;
    }
    let required_target = decision.required_target.as_deref()?;
    let normalized = normalize_target_path(required_target, workspace_root)?;
    if !is_code_or_test_target(&normalized) && !is_documentation_target(&normalized) {
        return None;
    }
    let filtered = filter_verification_repair_targets(
        prioritize_repair_targets(vec![normalized]),
        workspace_root,
    );
    (!filtered.is_empty()).then_some(filtered)
}

fn extract_verification_scope_targets(
    command: Option<&str>,
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let Some(command) = command else {
        return Vec::new();
    };

    let mut targets = Vec::new();
    for token in command.split_whitespace() {
        if let Some(target) = resolve_verification_target_token(token, workspace_root) {
            targets.push(target);
        }
    }
    prioritize_repair_targets(targets)
}

fn extract_verification_failure_labels(summary: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("verification failed:") {
            let label_segment = rest
                .split_once("; latest detail:")
                .map(|(labels, _)| labels)
                .unwrap_or(rest);
            for label in label_segment
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !labels.iter().any(|existing| existing == label) {
                    labels.push(label.to_string());
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("FAIL: ") {
            labels.push(compact_verification_failure_label(rest));
        } else if let Some(rest) = trimmed.strip_prefix("ERROR: ") {
            labels.push(compact_verification_failure_label(rest));
        } else if trimmed.starts_with("test_")
            && (trimmed.contains("... FAIL") || trimmed.contains("... ERROR"))
        {
            labels.push(
                trimmed
                    .split_whitespace()
                    .next()
                    .unwrap_or(trimmed)
                    .to_string(),
            );
        }
        if labels.len() >= MAX_VERIFICATION_FAILURE_LABELS {
            break;
        }
    }
    labels
}

fn compact_verification_failure_label(label: &str) -> String {
    label
        .split_whitespace()
        .next()
        .unwrap_or(label)
        .trim_matches(|ch| matches!(ch, '(' | ')' | ',' | ';'))
        .to_string()
}

fn resolve_verification_target_token(
    token: &str,
    workspace_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    let candidate = token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ','
            )
        })
        .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';' | '!' | '?'));
    if candidate.is_empty() || candidate.starts_with('-') {
        return None;
    }

    let lower = candidate.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "python" | "python.exe" | "py" | "-m" | "unittest" | "pytest" | "discover"
    ) {
        return None;
    }

    if candidate.contains('/') || candidate.contains('\\') || lower.ends_with(".py") {
        return normalize_target_path(candidate, workspace_root)
            .filter(|path| workspace_root.join(path).exists());
    }

    if !(lower.starts_with("test") || lower.contains("integration")) {
        return None;
    }

    let module_path = candidate.replace('.', "/");
    for possibility in [
        format!("{module_path}.py"),
        format!("{candidate}.py"),
        format!("tests/{module_path}.py"),
        format!("tests/{candidate}.py"),
    ] {
        if let Some(path) = normalize_target_path(&possibility, workspace_root)
            .filter(|path| workspace_root.join(path).exists())
        {
            return Some(path);
        }
    }

    None
}

fn tool_calls_by_call_id(
    transcript: &Transcript,
) -> HashMap<crate::session::ToolCallId, ToolCallMeta> {
    let mut tool_calls = HashMap::new();
    for message in &transcript.messages {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                tool_calls.insert(
                    value.tool_call_id,
                    ToolCallMeta {
                        tool_name: value.tool_name,
                        arguments_json: value.arguments_json.clone(),
                    },
                );
            }
        }
    }
    tool_calls
}

fn requested_work_discipline_from_history_items(
    workspace_root: &Utf8Path,
    history_items: &[HistoryItem],
    latest_user_text: Option<&str>,
    verification_commands: &[String],
    _workspace_blocked_reason: Option<&str>,
) -> RequestedWorkDiscipline {
    let Some(user_text) = latest_user_text else {
        return RequestedWorkDiscipline::default();
    };
    let mut protected_targets = extract_protected_artifact_targets(user_text);
    let requested_contract = requested_work_contract_from_instruction_text(user_text);
    for reference in &requested_contract.reference_inputs {
        if is_scenario_contract_ref(reference)
            && !protected_targets
                .iter()
                .any(|target| target.eq_ignore_ascii_case(reference))
        {
            protected_targets.push(reference.clone());
        }
    }
    let reference_inputs = requested_contract
        .reference_inputs
        .iter()
        .filter_map(|target| normalize_target_path(target, workspace_root))
        .collect::<Vec<_>>();
    let mut required_targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        latest_user_text,
    )
    .into_iter()
    .filter(|target| {
        !protected_targets
            .iter()
            .any(|protected| protected.eq_ignore_ascii_case(target.as_str()))
    })
    .collect::<Vec<_>>();
    if required_targets.is_empty() && same_document_update_alias_requested(user_text) {
        required_targets.extend(
            latest_authored_document_targets_before_latest_user(history_items, workspace_root)
                .into_iter()
                .filter(|target| {
                    !protected_targets
                        .iter()
                        .any(|protected| protected.eq_ignore_ascii_case(target.as_str()))
                }),
        );
    }
    let observed_written_targets =
        observed_written_targets_since_latest_user_history_items(history_items, workspace_root);
    let target_mutation_request =
        latest_user_requests_target_mutation(user_text, &required_targets);
    let pending_targets = required_targets
        .iter()
        .filter(|target| {
            let changed_after_latest_user =
                observed_target_set_contains(&observed_written_targets, target);
            if changed_after_latest_user {
                return false;
            }
            target_mutation_request || !workspace_root.join(target.as_str()).exists()
        })
        .cloned()
        .collect::<Vec<_>>();

    RequestedWorkDiscipline {
        required_targets,
        reference_inputs,
        protected_targets: protected_targets
            .into_iter()
            .filter_map(|target| normalize_target_path(&target, workspace_root))
            .collect(),
        verification_commands: verification_commands.to_vec(),
        pending_targets,
    }
}

fn latest_user_requests_target_mutation(user_text: &str, required_targets: &[Utf8PathBuf]) -> bool {
    if required_targets.is_empty() {
        return false;
    }
    let normalized = user_text.to_ascii_lowercase();
    const MUTATION_MARKERS: &[&str] = &[
        "update",
        "modify",
        "change",
        "edit",
        "revise",
        "implement",
        "add",
        "extend",
        "support",
        "refactor",
        "fix",
        "変更",
        "更新",
        "修正",
        "実装",
        "追加",
        "拡張",
        "対応",
        "扱える",
        "直して",
    ];
    MUTATION_MARKERS
        .iter()
        .any(|marker| normalized.contains(&marker.to_ascii_lowercase()))
}

fn latest_user_text_from_history_items(history_items: &[HistoryItem]) -> Option<String> {
    history_items_in_sequence(history_items)
        .into_iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::UserTurn { content, .. } => Some(content_text(content)),
            HistoryItemPayload::Message {
                role: MessageRole::User,
                content,
                ..
            } => Some(content_text(content)),
            _ => None,
        })
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn requested_deliverable_targets_from_instruction_text_for_workspace(
    workspace_root: &Utf8Path,
    latest_user_text: Option<&str>,
) -> Vec<Utf8PathBuf> {
    let Some(text) = latest_user_text else {
        return Vec::new();
    };
    let mut targets = BTreeSet::new();
    let contract = requested_work_contract_from_instruction_text(text);
    for target in contract.deliverable_targets {
        if let Some(normalized) = normalize_target_path(&target, workspace_root) {
            targets.insert(normalized.as_str().to_string());
        }
    }
    for artifact in staged_task_artifact_targets_from_text(text) {
        let path = workspace_root.join(&artifact);
        let Ok(content) = fs::read_to_string(path.as_std_path()) else {
            continue;
        };
        let contract = requested_work_contract_from_instruction_text(&content);
        for target in contract.deliverable_targets {
            if let Some(normalized) = normalize_target_path(&target, workspace_root) {
                targets.insert(normalized.as_str().to_string());
            }
        }
    }
    targets.into_iter().map(Utf8PathBuf::from).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocsRouteContract {
    instruction_text: String,
    deliverables: Vec<Utf8PathBuf>,
    required_areas: Vec<DocsArea>,
}

fn docs_route_contract_from_history_items(
    workspace_root: &Utf8Path,
    history_items: &[HistoryItem],
) -> Option<DocsRouteContract> {
    let latest_user = latest_user_text_from_history_items(history_items)?;
    let mut combined = latest_user.clone();
    for artifact in staged_task_artifact_targets_from_text(&latest_user) {
        let path = workspace_root.join(&artifact);
        let Ok(content) = fs::read_to_string(path.as_std_path()) else {
            continue;
        };
        combined.push('\n');
        combined.push_str(&content);
    }
    let mut requested_deliverables =
        requested_deliverable_targets_from_instruction_text_for_workspace(
            workspace_root,
            Some(&latest_user),
        )
        .into_iter()
        .collect::<Vec<_>>();
    if requested_deliverables.is_empty() && same_document_update_alias_requested(&latest_user) {
        requested_deliverables.extend(
            latest_authored_document_targets_before_latest_user(history_items, workspace_root)
                .into_iter()
                .filter(|target| is_documentation_target(target.as_path())),
        );
    }
    if requested_deliverables.is_empty()
        || requested_deliverables.iter().any(|target| {
            !is_documentation_target(target.as_path())
                && !docs_route_non_output_reference_target(
                    workspace_root,
                    &latest_user,
                    target.as_path(),
                )
        })
    {
        return None;
    }
    let deliverables = requested_deliverables
        .into_iter()
        .filter(|target| is_documentation_target(target.as_path()))
        .collect::<Vec<_>>();
    if deliverables.is_empty() {
        return None;
    }
    if let Some(prior_docs_contract) = prior_docs_route_contract_text_for_closeout_continuation(
        workspace_root,
        history_items,
        &latest_user,
        &deliverables,
    ) {
        combined.push('\n');
        combined.push_str(&prior_docs_contract);
    }
    if !looks_like_docs_only_route_contract(&combined, &deliverables) {
        return None;
    }
    let required_areas = docs_required_areas_from_instruction_text(&combined);
    Some(DocsRouteContract {
        instruction_text: combined,
        deliverables,
        required_areas,
    })
}

fn prior_docs_route_contract_text_for_closeout_continuation(
    workspace_root: &Utf8Path,
    history_items: &[HistoryItem],
    latest_user: &str,
    deliverables: &[Utf8PathBuf],
) -> Option<String> {
    if !looks_like_docs_closeout_continuation(latest_user, deliverables) {
        return None;
    }
    let latest_sequence = latest_user_turn_sequence(history_items)?;
    history_items_in_sequence(history_items)
        .into_iter()
        .rev()
        .filter(|item| history_item_order_scalar(item) < latest_sequence)
        .filter_map(history_item_user_text)
        .find(|text| {
            let mut combined = text.clone();
            for artifact in staged_task_artifact_targets_from_text(&text) {
                let path = workspace_root.join(&artifact);
                if let Ok(content) = fs::read_to_string(path.as_std_path()) {
                    combined.push('\n');
                    combined.push_str(&content);
                }
            }
            let requested = requested_deliverable_targets_from_instruction_text_for_workspace(
                workspace_root,
                Some(text.as_str()),
            );
            let docs_requested = requested
                .into_iter()
                .filter(|target| is_documentation_target(target.as_path()))
                .collect::<Vec<_>>();
            !docs_requested.is_empty()
                && deliverables.iter().all(|deliverable| {
                    docs_requested.iter().any(|prior| {
                        docs_route_target_alias_matches(prior.as_str(), deliverable.as_str())
                    })
                })
                && looks_like_docs_only_route_contract(&combined, &docs_requested)
        })
}

fn looks_like_docs_closeout_continuation(text: &str, deliverables: &[Utf8PathBuf]) -> bool {
    if deliverables.is_empty()
        || !deliverables
            .iter()
            .all(|target| is_documentation_target(target.as_path()))
    {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let closeout_signal = lower.contains("manual st closeout continuation")
        || lower.contains("stop-hook continuation")
        || lower.contains("missing expected artifacts")
        || lower.contains("open obligations");
    let docs_repair_signal = lower.contains("repair docs")
        || lower.contains("docs deliverable")
        || text.contains("ドキュメント");
    closeout_signal
        && docs_repair_signal
        && deliverables
            .iter()
            .any(|target| lower.contains(&target.as_str().to_ascii_lowercase()))
}

fn history_item_user_text(item: &HistoryItem) -> Option<String> {
    match &item.payload {
        HistoryItemPayload::UserTurn { content, .. } => Some(content_text(content)),
        HistoryItemPayload::Message {
            role: MessageRole::User,
            content,
            ..
        } => Some(content_text(content)),
        _ => None,
    }
}

fn docs_route_non_output_reference_target(
    workspace_root: &Utf8Path,
    text: &str,
    target: &Utf8Path,
) -> bool {
    staged_task_artifact_targets_from_text(text)
        .into_iter()
        .any(|artifact| docs_route_target_alias_matches(target.as_str(), &artifact))
        || extract_protected_artifact_targets(text)
            .into_iter()
            .any(|artifact| docs_route_target_alias_matches(target.as_str(), &artifact))
        || workspace_root.join(target).exists()
}

fn docs_route_target_alias_matches(left: &str, right: &str) -> bool {
    let left = left
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase();
    let right = right
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase();
    left == right || left.ends_with(&format!("/{right}")) || right.ends_with(&format!("/{left}"))
}

fn looks_like_docs_only_route_contract(text: &str, deliverables: &[Utf8PathBuf]) -> bool {
    let has_docs_signal = docs_route_has_docs_signal(text);
    let has_no_code_mutation_signal = docs_route_has_no_code_mutation_signal(text);
    has_docs_signal
        && has_no_code_mutation_signal
        && deliverables
            .iter()
            .all(|target| is_documentation_target(target.as_path()))
}

fn docs_route_has_docs_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("docs-only")
        || lower.contains("documentation")
        || lower.contains("document")
        || lower.contains("readme")
        || text.contains("文書のみ")
        || text.contains("文書化")
        || text.contains("設計")
        || text.contains("ドキュメント")
}

fn docs_route_has_no_code_mutation_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("do not change")
        || lower.contains("don't change")
        || lower.contains("without changing")
        || lower.contains("docs-only")
        || text.contains("変更しない")
        || text.contains("変更せず")
        || text.contains("文書のみ")
        || text.contains("文書だけ")
}

fn build_docs_route_state(
    workspace_root: &Utf8Path,
    contract: &DocsRouteContract,
) -> DocsRouteState {
    let area_coverage = contract
        .required_areas
        .iter()
        .copied()
        .map(|area| {
            let representative_paths = collect_docs_area_representative_paths(workspace_root, area);
            DocsAreaCoverage {
                area,
                status: if representative_paths.is_empty() {
                    ContractStatus::Pending
                } else {
                    ContractStatus::Satisfied
                },
                representative_paths,
                evidence_summary: Some(format!(
                    "{} area representative path coverage",
                    docs_area_label(area)
                )),
            }
        })
        .collect::<Vec<_>>();
    let factual_checks = docs_route_factual_checks(workspace_root, &contract.instruction_text);
    let deliverables = contract
        .deliverables
        .iter()
        .map(|target| {
            docs_deliverable_coverage(
                workspace_root,
                target.clone(),
                docs_deliverable_kind(target),
                &contract.required_areas,
            )
        })
        .collect::<Vec<_>>();
    let active_deliverable = deliverables
        .iter()
        .find(|deliverable| {
            docs_route_missing_coverage_summary(deliverable).is_some()
                || !workspace_root.join(deliverable.target.as_str()).exists()
        })
        .map(|deliverable| deliverable.target.clone())
        .or_else(|| contract.deliverables.first().cloned());
    let pending_deliverables = docs_route_pending_deliverables_from_parts(
        &area_coverage,
        &deliverables,
        &factual_checks,
        active_deliverable.as_ref(),
    );
    DocsRouteState {
        active_deliverable,
        pending_deliverables,
        survey_packet_summary: Some(docs_route_survey_packet_summary(&contract.required_areas)),
        area_coverage,
        deliverables,
        factual_checks,
    }
}

fn docs_deliverable_kind(target: &Utf8Path) -> DocsDeliverableKind {
    match target
        .file_name()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "readme.md" => DocsDeliverableKind::Readme,
        "basic_design.md" => DocsDeliverableKind::BasicDesign,
        "detail_design.md" => DocsDeliverableKind::DetailDesign,
        _ => DocsDeliverableKind::Other,
    }
}

fn docs_deliverable_coverage(
    workspace_root: &Utf8Path,
    target: Utf8PathBuf,
    kind: DocsDeliverableKind,
    required_areas: &[DocsArea],
) -> DocsDeliverableCoverage {
    let path = workspace_root.join(target.as_str());
    let content = fs::read_to_string(path.as_std_path()).unwrap_or_default();
    let representative_paths =
        docs_representative_paths_mentioned_in_text(workspace_root, &content);
    DocsDeliverableCoverage {
        target,
        kind,
        required_areas: required_areas.to_vec(),
        required_topics: docs_required_topics(kind, required_areas),
        satisfied_topics: docs_satisfied_topics(kind, &content, required_areas),
        representative_paths,
        grounding: docs_grounding_coverage(workspace_root, required_areas),
    }
}

fn docs_required_areas_from_instruction_text(text: &str) -> Vec<DocsArea> {
    let lower = text.to_ascii_lowercase();
    docs_area_catalog()
        .into_iter()
        .filter(|area| {
            docs_area_markers(*area)
                .iter()
                .any(|marker| lower.contains(&marker.to_ascii_lowercase()) || text.contains(marker))
        })
        .collect()
}

fn docs_required_topics(kind: DocsDeliverableKind, required_areas: &[DocsArea]) -> Vec<String> {
    let topics: &[&str] = match kind {
        DocsDeliverableKind::Readme => &["overview"],
        DocsDeliverableKind::BasicDesign => &["architecture", "responsibility", "data flow"],
        DocsDeliverableKind::DetailDesign => &["module input output", "data model", "flow"],
        DocsDeliverableKind::Other => &["repository evidence"],
    };
    let mut merged = topics
        .iter()
        .copied()
        .map(str::to_string)
        .collect::<Vec<_>>();
    merged.extend(
        required_areas
            .iter()
            .map(|area| docs_area_label(*area).to_string()),
    );
    merged
}

fn docs_satisfied_topics(
    kind: DocsDeliverableKind,
    content: &str,
    required_areas: &[DocsArea],
) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    docs_required_topics(kind, required_areas)
        .into_iter()
        .filter(|topic| docs_topic_is_satisfied(topic, &lower, content))
        .collect()
}

fn docs_topic_is_satisfied(topic: &str, lower: &str, original: &str) -> bool {
    match topic {
        "overview" => lower.contains("overview") || original.contains("概要"),
        "architecture" => lower.contains("architecture") || original.contains("アーキテクチャ"),
        "responsibility" => lower.contains("responsibility") || original.contains("責務"),
        "data flow" => lower.contains("data flow") || original.contains("データフロー"),
        "module input output" => {
            lower.contains("input") && lower.contains("output")
                || original.contains("入出力")
                || original.contains("入力") && original.contains("出力")
        }
        "data model" => {
            lower.contains("data model")
                || lower.contains("data-model")
                || lower.contains("schema")
                || original.contains("主要データ")
                || original.contains("データモデル")
                || original.contains("データ構造")
                || original.contains("データ定義")
                || original.contains("データスキーマ")
        }
        "flow" => lower.contains("flow") || original.contains("フロー"),
        "repository evidence" => {
            lower.contains(".md") || lower.contains("/") || original.contains("実装")
        }
        "backend" => docs_topic_area_marker_is_satisfied(DocsArea::Backend, lower, original),
        "frontend" => docs_topic_area_marker_is_satisfied(DocsArea::Frontend, lower, original),
        "tests" => docs_topic_area_marker_is_satisfied(DocsArea::Tests, lower, original),
        "data" => docs_topic_area_marker_is_satisfied(DocsArea::Data, lower, original),
        "examples" => docs_topic_area_marker_is_satisfied(DocsArea::Examples, lower, original),
        other => lower.contains(other),
    }
}

fn docs_topic_area_marker_is_satisfied(area: DocsArea, lower: &str, original: &str) -> bool {
    docs_area_markers(area)
        .iter()
        .any(|marker| lower.contains(&marker.to_ascii_lowercase()) || original.contains(marker))
}

fn docs_grounding_coverage(
    workspace_root: &Utf8Path,
    required_areas: &[DocsArea],
) -> Vec<DocsGroundingCoverage> {
    let mut coverage = docs_grounding_requirements_for_areas(workspace_root, required_areas)
        .into_iter()
        .map(|(requirement, candidates)| {
            let representative_path = candidates
                .into_iter()
                .map(Utf8PathBuf::from)
                .find(|candidate| workspace_root.join(candidate.as_str()).exists());
            DocsGroundingCoverage {
                requirement,
                status: if representative_path.is_some() {
                    ContractStatus::Satisfied
                } else {
                    ContractStatus::Pending
                },
                representative_path,
                evidence_summary: Some(docs_grounding_requirement_label(requirement).to_string()),
            }
        })
        .collect::<Vec<_>>();
    if required_areas.contains(&DocsArea::Tests)
        && !coverage
            .iter()
            .any(|item| item.requirement == DocsGroundingRequirement::Tests)
        && let Some(path) = collect_docs_area_representative_paths(workspace_root, DocsArea::Tests)
            .into_iter()
            .next()
    {
        coverage.push(DocsGroundingCoverage {
            requirement: DocsGroundingRequirement::Tests,
            status: ContractStatus::Satisfied,
            representative_path: Some(path),
            evidence_summary: Some(
                docs_grounding_requirement_label(DocsGroundingRequirement::Tests).to_string(),
            ),
        });
    }
    coverage
}

fn docs_route_factual_checks(
    workspace_root: &Utf8Path,
    instruction_text: &str,
) -> Vec<DocsFactCheck> {
    let mut checks = Vec::new();
    if instruction_text.to_ascii_lowercase().contains("task.md") {
        checks.push(("task", DocsFactCheckKind::PathExists, "task.md"));
    }
    checks
        .into_iter()
        .map(|(label, kind, subject)| {
            let path = Utf8PathBuf::from(subject);
            DocsFactCheck {
                label: label.to_string(),
                kind,
                subject: subject.to_string(),
                status: if workspace_root.join(path.as_str()).exists() {
                    ContractStatus::Satisfied
                } else {
                    ContractStatus::Pending
                },
                evidence_summary: Some(format!("{subject} existence check")),
            }
        })
        .collect()
}

pub(crate) fn docs_route_contract_promotes_docs_repair_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for dir in [
        "backend/app/api",
        "frontend/app",
        "backend/tests",
        "data",
        "examples",
    ] {
        if fs::create_dir_all(workspace.join(dir).as_std_path()).is_err() {
            return false;
        }
    }
    let files = [
        ("backend/pyproject.toml", "[project]\nname = \"demo\"\n"),
        ("backend/app/main.py", "source"),
        ("frontend/package.json", "{}"),
        ("frontend/app/page.tsx", "source"),
        ("backend/tests/test_api.py", "test"),
        ("data/sample.json", "{}"),
        ("examples/demo.py", "example"),
        (
            "task.md",
            r#"
制約:
- 既存の実装コード、設定、テストは変更しないこと。今回の成果物は文書のみとすること。
- build artifact、cache、generated output、dependency を無差別に読まないこと。

Step2: `README.md` を作成する。
Step3: `basic_design.md` を作成する。
Step4: `detail_design.md` を作成する。
backend / frontend / tests / data / examples の実装実態と整合させる。
"#,
        ),
        (
            "README.md",
            "overview backend frontend tests data examples backend/app frontend/app backend/tests data examples",
        ),
        (
            "basic_design.md",
            "architecture responsibility data flow backend frontend backend/app frontend/app tests data examples",
        ),
    ];
    for (path, content) in files {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "docs route".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "current directory の `task.md` に従って documentation task を実施してください。".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut previous = SessionStateSnapshot::default();
    previous.route = TaskRoute::Code;
    previous.process_phase = ProcessPhase::Author;
    previous.active_targets = vec![Utf8PathBuf::from("docs/widget-design.md")];
    previous.completion.open_work_count = 1;
    previous.completion.blocked_reason = Some(
        "Requested deliverables still require authoring in the workspace: `docs/widget-design.md`."
            .to_string(),
    );
    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && state.docs_route.is_some()
        && matches!(active, Some(ActiveWorkContract::DocsRepair { .. }))
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "detail_design.md")
        && docs_route_path_is_generated_or_dependency(Utf8Path::new(
            "frontend/node_modules/pkg/index.js",
        ))
        && docs_route_path_is_generated_or_dependency(Utf8Path::new("backend/__pycache__/mod.pyc"))
}

pub(crate) fn docs_route_contract_does_not_require_unmentioned_web_areas_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for (path, content) in [
        (
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        ),
        ("src/main.rs", "fn main() {}\n"),
    ] {
        if let Some(parent) = workspace.join(path).parent()
            && fs::create_dir_all(parent.as_std_path()).is_err()
        {
            return false;
        }
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "generic docs route".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Create repository documentation only. Create `README.md`. Create `basic_design.md`. Create `detail_design.md`. Do not change source code.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let Some(docs) = state.docs_route.as_ref() else {
        return false;
    };
    let pending_summary = docs
        .pending_deliverables
        .iter()
        .map(|item| item.summary.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    state.route == TaskRoute::Docs
        && docs.area_coverage.is_empty()
        && docs.factual_checks.is_empty()
        && docs
            .deliverables
            .iter()
            .all(|deliverable| deliverable.required_areas.is_empty())
        && !pending_summary.contains("backend")
        && !pending_summary.contains("frontend")
        && !pending_summary.contains("examples")
        && !pending_summary.contains("data artifact")
        && !pending_summary.contains("test file")
}

pub(crate) fn docs_route_single_deliverable_contract_promotes_docs_repair_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for (path, content) in [
        (
            "component.py",
            "def calculate(left, operator, right):\n    return left + right\n",
        ),
        (
            "test_component.py",
            "import unittest\nimport component\n\nclass ComponentTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.calculate(2, '+', 3), 5)\n",
        ),
    ] {
        if let Some(parent) = workspace.join(path).parent()
            && fs::create_dir_all(parent.as_std_path()).is_err()
        {
            return false;
        }
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "single docs deliverable".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let change_id = ChangeId::new();
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
                    text: "現在の実装を調査し、`docs/component-design.md` を日本語で作成してください。実装コードと test は変更せず、確認できた事実だけを文書化してください。最後に `python -m unittest` を実行してください。".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![change_id],
                changes: vec![FileChangeEvidence {
                    change_id,
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/component-design.md")),
                    summary: "Added docs/component-design.md".to_string(),
                }],
                summary: "Added docs/component-design.md".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let passed = state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && state.docs_route.is_some()
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/component-design.md")
        && matches!(active, Some(ActiveWorkContract::DocsRepair { .. }));
    passed
}

pub(crate) fn docs_route_flat_test_artifact_satisfies_required_area_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for (path, content) in [
        (
            "component.py",
            "def calculate(left, operator, right):\n    return left + right\n",
        ),
        (
            "test_component.py",
            "import unittest\nimport component\n\nclass ComponentTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(component.calculate(2, '+', 3), 5)\n",
        ),
        (
            "docs/component-design.md",
            "# コンポーネント設計\n\n## 概要\n\n実装 `component.py` と `test_component.py` を確認した事実を記録します。\n\n## テスト\n\nroot-level `test_component.py` は `unittest` で `component.calculate` を検証します。\n",
        ),
    ] {
        if let Some(parent) = workspace.join(path).parent()
            && fs::create_dir_all(parent.as_std_path()).is_err()
        {
            return false;
        }
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "flat docs test evidence".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "現在の実装を調査し、`docs/component-design.md` を日本語で作成してください。実装コードと test は変更せず、確認できた事実だけを文書化してください。最後に `python -m unittest` を実行してください。".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let Some(docs) = state.docs_route.as_ref() else {
        return false;
    };
    let tests_area_satisfied = docs.area_coverage.iter().any(|coverage| {
        coverage.area == DocsArea::Tests
            && coverage.status == ContractStatus::Satisfied
            && coverage
                .representative_paths
                .iter()
                .any(|path| path.as_str() == "test_component.py")
    });
    let deliverable_tests_satisfied = docs.deliverables.iter().any(|deliverable| {
        deliverable.target.as_str() == "docs/component-design.md"
            && docs_deliverable_missing_required_areas(deliverable).is_empty()
    });
    state.route == TaskRoute::Docs
        && tests_area_satisfied
        && deliverable_tests_satisfied
        && !state.completion.route_contract_pending
        && docs.pending_deliverables.is_empty()
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command.contains("unittest"))
}

pub(crate) fn docs_route_localized_topic_completion_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for dir in [
        "backend/app/api",
        "frontend/app",
        "backend/tests",
        "data",
        "examples",
    ] {
        if fs::create_dir_all(workspace.join(dir).as_std_path()).is_err() {
            return false;
        }
    }
    let files = [
        ("backend/app/main.py", "source"),
        ("frontend/app/page.tsx", "source"),
        ("backend/tests/test_api.py", "test"),
        ("data/sample.json", "{}"),
        ("examples/demo.py", "example"),
        (
            "task.md",
            r#"
制約:
- 既存の実装コード、設定、テストは変更しないこと。今回の成果物は文書のみとすること。

Step2: `README.md` を作成する。
Step3: `basic_design.md` を作成する。
Step4: `detail_design.md` を作成する。
backend / frontend / tests / data / examples の実装実態と整合させる。
"#,
        ),
        (
            "README.md",
            "概要 backend frontend tests data examples backend/app frontend/app backend/tests data examples",
        ),
        (
            "basic_design.md",
            "アーキテクチャ 責務 データフロー backend frontend backend/app frontend/app tests data examples",
        ),
        (
            "detail_design.md",
            "入出力\n## データモデル\nフロー backend frontend backend/app frontend/app backend/tests data examples",
        ),
    ];
    for (path, content) in files {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    for localized in ["データモデル", "データ構造", "データ定義", "データスキーマ"]
    {
        let required_areas = [DocsArea::Backend, DocsArea::Frontend];
        let topics = docs_satisfied_topics(
            DocsDeliverableKind::DetailDesign,
            &format!("入出力\n## {localized}\nフロー backend frontend"),
            &required_areas,
        );
        if !topics.iter().any(|topic| topic == "data model") {
            return false;
        }
    }

    let coverage = docs_deliverable_coverage(
        workspace.as_path(),
        Utf8PathBuf::from("detail_design.md"),
        DocsDeliverableKind::DetailDesign,
        &[
            DocsArea::Backend,
            DocsArea::Frontend,
            DocsArea::Tests,
            DocsArea::Data,
            DocsArea::Examples,
        ],
    );
    coverage
        .satisfied_topics
        .iter()
        .any(|topic| topic == "data model")
}

fn explicit_required_verification_commands_from_history_items(
    workspace_root: &Utf8Path,
    latest_user_text: Option<&str>,
) -> Vec<String> {
    let mut commands = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(text) = latest_user_text {
        for command in explicit_verification_commands_from_text(text) {
            let key = canonical_verification_command_identity_key(&command)
                .unwrap_or_else(|| command.to_ascii_lowercase());
            if seen.insert(key) {
                commands.push(command);
            }
        }
        for artifact in staged_task_artifact_targets_from_text(text) {
            let path = workspace_root.join(&artifact);
            let Ok(content) = fs::read_to_string(path.as_std_path()) else {
                continue;
            };
            for command in explicit_verification_commands_from_text(&content) {
                let key = canonical_verification_command_identity_key(&command)
                    .unwrap_or_else(|| command.to_ascii_lowercase());
                if seen.insert(key) {
                    commands.push(command);
                }
            }
        }
    }

    commands
}

fn observed_written_targets_since_latest_user_history_items(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for item in history_items_since_latest_user_turn(history_items) {
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                        && let Some(normalized) =
                            normalize_target_path(path.as_str(), workspace_root)
                                .filter(|path| is_authoring_content_change_path(path.as_path()))
                    {
                        targets.insert(normalized.as_str().to_ascii_lowercase());
                    }
                }
            }
            HistoryItemPayload::ToolOutput { metadata, .. } => {
                for path in changed_paths_from_tool_output_metadata(metadata) {
                    if let Some(normalized) = normalize_target_path(&path, workspace_root)
                        .filter(|path| is_authoring_content_change_path(path.as_path()))
                    {
                        targets.insert(normalized.as_str().to_ascii_lowercase());
                    }
                }
            }
            _ => {}
        }
    }
    targets
}

fn latest_authored_document_targets_before_latest_user(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let Some(latest_user_sequence) = latest_user_turn_sequence(history_items) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for item in history_items_in_sequence(history_items).into_iter().rev() {
        if history_item_order_scalar(item) >= latest_user_sequence {
            continue;
        }
        let mut item_paths = Vec::new();
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref()) {
                        item_paths.push(path.as_str().to_string());
                    }
                }
            }
            HistoryItemPayload::ToolOutput { metadata, .. } => {
                item_paths.extend(changed_paths_from_tool_output_metadata(metadata));
            }
            _ => {}
        }
        for path in item_paths {
            let Some(normalized) = normalize_target_path(&path, workspace_root) else {
                continue;
            };
            if !is_documentation_target(normalized.as_path())
                || is_scenario_contract_ref(normalized.as_str())
            {
                continue;
            }
            let key = normalized.as_str().to_ascii_lowercase();
            if seen.insert(key) {
                found.push(normalized);
            }
        }
        if !found.is_empty() {
            break;
        }
    }
    found
}

fn observed_written_targets_since_latest_verification_failure(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> BTreeSet<String> {
    let Some(latest_failure_sequence) =
        latest_verification_failure_authority_sequence(history_items)
    else {
        return BTreeSet::new();
    };

    let mut targets = BTreeSet::new();
    for item in history_items_in_sequence(history_items) {
        if history_item_order_scalar(item) <= latest_failure_sequence {
            continue;
        }
        if let HistoryItemPayload::FileChange { changes, .. } = &item.payload {
            for change in changes {
                if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                    && let Some(normalized) = normalize_target_path(path.as_str(), workspace_root)
                        .filter(|path| is_authoring_content_change_path(path.as_path()))
                {
                    targets.insert(normalized.as_str().to_ascii_lowercase());
                }
            }
        } else if let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload {
            for path in changed_paths_from_tool_output_metadata(metadata) {
                if let Some(normalized) = normalize_target_path(&path, workspace_root)
                    .filter(|path| is_authoring_content_change_path(path.as_path()))
                {
                    targets.insert(normalized.as_str().to_ascii_lowercase());
                }
            }
        }
    }
    targets
}

fn content_changing_progress_since_latest_verification_failure(
    history_items: &[HistoryItem],
) -> bool {
    let Some(latest_failure_sequence) =
        latest_verification_failure_authority_sequence(history_items)
    else {
        return false;
    };
    history_items_in_sequence(history_items).iter().any(|item| {
        if history_item_order_scalar(item) <= latest_failure_sequence {
            return false;
        }
        let HistoryItemPayload::ToolOutput {
            metadata,
            progress_effect,
            ..
        } = &item.payload
        else {
            return false;
        };
        *progress_effect == ToolProgressEffect::MadeProgress
            && metadata
                .get("operation_progress_class")
                .or_else(|| metadata.pointer("/tool_feedback_envelope/operation_progress_class"))
                .or_else(|| metadata.pointer("/tool_result_metadata/operation_progress_class"))
                .and_then(Value::as_str)
                == Some("content_changing_progress")
    })
}

fn latest_verification_failure_authority_sequence(history_items: &[HistoryItem]) -> Option<i64> {
    history_items
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::ToolOutput {
                status,
                verification_run: Some(run),
                ..
            } if *status == ToolLifecycleStatus::Completed
                && matches!(
                    run.status,
                    VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
                ) =>
            {
                Some(history_item_order_scalar(item))
            }
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::Message {
                role: MessageRole::User,
                content,
                ..
            } if looks_like_verification_repair_continuation_text(&content_text(content)) => {
                Some(history_item_order_scalar(item))
            }
            _ => None,
        })
        .max()
}

fn changed_paths_from_tool_output_metadata(metadata: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(content_evidence) = metadata.get("file_change_content_evidence") {
        if content_evidence
            .get("content_bearing")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Vec::new();
        }
        if let Some(content_bearing_paths) = content_evidence
            .get("content_bearing_paths")
            .and_then(Value::as_array)
        {
            paths.extend(
                content_bearing_paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
            return paths;
        }
    }
    if let Some(changes) = metadata.get("changes").and_then(Value::as_array) {
        for change in changes {
            if let Some(path) = change
                .get("path_after")
                .or_else(|| change.get("path_before"))
                .and_then(Value::as_str)
            {
                paths.push(path.to_string());
            }
        }
    }
    if let Some(changed_files) = metadata.get("changed_files").and_then(Value::as_array) {
        paths.extend(
            changed_files
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| !metadata_changed_file_value_is_opaque_id(path))
                .map(str::to_string),
        );
    }
    if let Some(tool_result_metadata) = metadata.get("tool_result_metadata") {
        paths.extend(changed_paths_from_tool_output_metadata(
            tool_result_metadata,
        ));
    }
    paths
}

fn file_changes_have_authoring_content_change(
    changes: &[FileChangeEvidence],
    workspace_root: &Utf8Path,
) -> bool {
    changes.iter().any(|change| {
        change
            .path_after
            .as_ref()
            .or(change.path_before.as_ref())
            .and_then(|path| normalize_target_path(path.as_str(), workspace_root))
            .is_some_and(|path| is_authoring_content_change_path(path.as_path()))
    })
}

fn metadata_has_authoring_content_change(metadata: &Value, workspace_root: &Utf8Path) -> bool {
    changed_paths_from_tool_output_metadata(metadata)
        .into_iter()
        .filter_map(|path| normalize_target_path(&path, workspace_root))
        .any(|path| is_authoring_content_change_path(path.as_path()))
}

fn is_authoring_content_change_path(path: &Utf8Path) -> bool {
    !path_is_verification_runner_byproduct_or_dependency(path)
}

fn path_is_verification_runner_byproduct_or_dependency(path: &Utf8Path) -> bool {
    let normalized = path.as_str().replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".pyc")
        || normalized.ends_with(".pyo")
        || normalized.ends_with(".pytest_cache")
        || normalized.split('/').any(|segment| {
            matches!(
                segment,
                ".git"
                    | ".hg"
                    | ".svn"
                    | ".moyai"
                    | ".venv"
                    | "venv"
                    | ".pytest_cache"
                    | ".ruff_cache"
                    | "__pycache__"
                    | "node_modules"
                    | "target"
                    | ".next"
                    | "dist"
                    | "build"
                    | "coverage"
                    | "playwright-report"
                    | "test-results"
            )
        })
}

fn metadata_changed_file_value_is_opaque_id(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() == 26
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('.')
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, 'A'..='Z'))
}

pub(crate) fn requested_work_missing_todo_graph_stays_authoring_authority() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "missing todo graph authoring authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Create `component.py` and then run `python -m unittest`.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Author
        && state.active_targets == vec![Utf8PathBuf::from("component.py")]
        && !state.completion.closeout_ready
        && !state.completion.verification_pending
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                verification_commands,
            }) if pending_targets == vec![Utf8PathBuf::from("component.py")]
                && verification_commands == vec!["python -m unittest".to_string()]
        )
}

pub(crate) fn partial_requested_work_remains_authoring_phase_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "partial authoring".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:\\workspace\\project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Added component.py".to_string(),
                }],
                summary: "Added component.py".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Author
        && !state.completion.closeout_ready
        && !state.completion.verification_pending
        && state.completion.open_work_count == 1
        && state.active_targets == vec![Utf8PathBuf::from("test_component.py")]
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("test_component.py")]
        )
}

pub(crate) fn verification_failure_labels_are_not_requested_work_targets_fixture_passes() -> bool {
    let text = r#"
Manual ST closeout continuation.

Open obligations:
- author `test_arcade_game.TestBulletClass.test_bullet_creation`
- author `test_arcade_game.TestBulletClass.test_bullet_destroy`

Required verification failed in the latest evidence:
- `python -m unittest`
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        Utf8Path::new("C:/workspace/project"),
        Some(text),
    );
    !targets.iter().any(|target| {
        target
            .as_str()
            .starts_with("test_arcade_game.TestBulletClass")
    })
}

pub(crate) fn verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes()
-> bool {
    let workspace_root =
        Utf8Path::new("C:/Users/example/Desktop/CodingAgent/project_sandbox/route/generic/workspace");
    let text = r#"
Verification repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Repair targets:
- component.py

Failed required verification commands:
- python -m unittest

Latest verification failure evidence:
- command: python -m unittest
stderr: Exception in thread Thread-2 (_readerthread):
Traceback (most recent call last):
  File "C:\Python313\Lib\threading.py", line 1043, in _bootstrap_inner
  File "C:\Python313\Lib\subprocess.py", line 1615, in _readerthread
  File "C:\Python313\Lib\unittest\case.py", line 1171, in assertIn
  File "C:\Users\example\Desktop\CodingAgent\project_sandbox\route\generic\workspace\test_component.py", line 158, in test_cli_divide_by_zero
TypeError: argument of type 'NoneType' is not iterable

Expected artifacts:
- component.py
- test_component.py

After the repair edit, rerun the failed required verification command(s) with shell.
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    let no_diagnostic_targets = targets.iter().all(|target| {
        let normalized = target.as_str().replace('\\', "/").to_ascii_lowercase();
        !normalized.contains("python313")
            && !normalized.contains("/lib/")
            && !normalized.contains("subprocess.py")
            && !normalized.contains("threading.py")
            && !normalized.contains("unittest/case.py")
            && !normalized.contains("/users/")
            && !normalized.contains("project_sandbox/route")
    });
    if !no_diagnostic_targets {
        return false;
    }
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification diagnostic path authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace_root.to_path_buf(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    state.process_phase == ProcessPhase::Repair
        && state.active_targets == vec![Utf8PathBuf::from("component.py")]
        && state.completion.verification_pending
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("verification failed"))
        && state.active_targets.iter().all(|target| {
            let normalized = target.as_str().replace('\\', "/").to_ascii_lowercase();
            !normalized.contains("python313")
                && !normalized.contains("/lib/")
                && !normalized.contains("/users/")
                && !normalized.contains("subprocess.py")
                && !normalized.contains("threading.py")
                && !normalized.contains("unittest/case.py")
        })
}

pub(crate) fn continuation_context_symbols_are_not_requested_work_targets_fixture_passes() -> bool {
    let workspace_root =
        Utf8Path::new("C:/Users/example/Desktop/CodingAgent/project_sandbox/route/generic/workspace");
    let text = r#"
Verification repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Previous final assistant message:
All tests passed.

- `component.py`: created a CLI component. It supports function mode (`parse_and_evaluate`) and CLI mode (`sys.argv`).
- `test_component.py`: created unit and CLI integration tests.

Repair targets:
- component.py

Failed required verification commands:
- python -m unittest

Expected artifacts:
- component.py
- test_component.py
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    if targets.iter().any(|target| {
        matches!(
            target.as_str(),
            "sys.argv" | "parse_and_evaluate" | "component.parse_and_evaluate"
        )
    }) {
        return false;
    }
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "continuation context symbol authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace_root.to_path_buf(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    state.active_targets == vec![Utf8PathBuf::from("component.py")]
        && state.active_targets.iter().all(|target| {
            !matches!(
                target.as_str(),
                "sys.argv" | "parse_and_evaluate" | "component.parse_and_evaluate"
            )
        })
}

pub(crate) fn manual_st_closeout_expected_artifacts_inventory_does_not_reopen_fixture_passes()
-> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let text = r#"
Manual ST closeout continuation.

The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete.

Open obligations:
- repair docs `docs/widget-design.md`

Missing expected artifacts:
- none

Expected artifacts:
- widget.py
- docs/widget-design.md
- test_widget.py

Expected artifacts are route inventory evidence only. They do not create new authoring targets unless the same path is listed under Open obligations or Missing expected artifacts.
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    if targets != vec![Utf8PathBuf::from("docs/widget-design.md")] {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "manual ST closeout inventory authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace_root.to_path_buf(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    state.active_targets == vec![Utf8PathBuf::from("docs/widget-design.md")]
        && !state
            .active_targets
            .iter()
            .any(|target| matches!(target.as_str(), "widget.py" | "test_widget.py"))
}

pub(crate) fn docs_route_closeout_continuation_preserves_docs_authority_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace.join("calculator.py").as_std_path(),
            "def add(a, b):\n    return a + b\n",
        )
        .is_err()
        || fs::write(
            workspace.join("test_calculator.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
        || fs::write(
            workspace.join("scenario_contract.md").as_std_path(),
            "# Contract\n",
        )
        .is_err()
        || fs::write(
            workspace.join("scenario_contract.json").as_std_path(),
            "{}\n",
        )
        .is_err()
    {
        return false;
    }
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "docs closeout continuation".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let initial_request = r#"
現在の実装を調査し、`docs/calculator-design.md` を日本語で作成してください。
実装コードと test は変更せず、確認できた事実だけを文書化してください。
最後に `python -m unittest` を実行してください。

Scenario contract authority:
- `scenario_contract.md`
- `scenario_contract.json`
"#;
    let continuation = r#"
Manual ST closeout continuation.

The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete.

Open obligations:
- repair docs `docs/calculator-design.md`
- repair docs deliverable `docs/calculator-design.md`

Missing expected artifacts:
- docs/calculator-design.md

Expected artifacts:
- calculator.py
- docs/calculator-design.md
- test_calculator.py

Expected artifacts are route inventory evidence only. They do not create new authoring targets unless the same path is listed under Open obligations or Missing expected artifacts.
"#;
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
                    text: initial_request.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: TurnId::new(),
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: continuation.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.route == TaskRoute::Docs
        && state.docs_route.is_some()
        && state.completion.route_contract_pending
        && state.active_targets == vec![Utf8PathBuf::from("docs/calculator-design.md")]
        && matches!(active, Some(ActiveWorkContract::DocsRepair { .. }))
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("docs route contract is pending"))
}

pub(crate) fn verification_repair_continuation_projects_repair_state_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace.join("widget.py").as_std_path(),
            "def render(value):\n    return str(value)\n",
        )
        .is_err()
        || fs::write(
            workspace.join("test_widget.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
        || fs::write(
            workspace.join("docs/widget-contract.md").as_std_path(),
            "# Widget contract\n",
        )
        .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification repair continuation projection".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Manual ST verification-repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Previous final assistant message:
All tests passed.

Repair targets:
- widget.py

Failed required verification commands:
- python -m unittest

Latest verification failure evidence:
- command: python -m unittest
- public command contract: stderr did not include an accepted usage marker for invalid arguments.

Expected artifacts:
- widget.py
- test_widget.py
- docs/widget-contract.md

After the repair edit, rerun the failed required verification command(s) with shell.
"#;
    let initial_items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: continuation.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let initial_state = reduce_session_state_from_history_items(
        &session,
        &initial_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let initial_active =
        active_work_contract_for_history_items(&session, &initial_items, &initial_state, &[]);
    let initial_ok = initial_state.process_phase == ProcessPhase::Repair
        && initial_state.active_targets == vec![Utf8PathBuf::from("widget.py")]
        && initial_state.completion.open_work_count == 0
        && initial_state.completion.verification_pending
        && initial_state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("verification failed"))
        && initial_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            initial_active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("widget.py")]
        );

    let mut repaired_items = initial_items;
    repaired_items.push(HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::FileChange {
            change_ids: vec![ChangeId::new()],
            changes: vec![FileChangeEvidence {
                change_id: ChangeId::new(),
                kind: crate::session::ChangeKind::Update,
                path_before: Some(Utf8PathBuf::from("widget.py")),
                path_after: Some(Utf8PathBuf::from("widget.py")),
                summary: "Updated widget.py".to_string(),
            }],
            summary: "Updated widget.py".to_string(),
        },
    });
    let repaired_state = reduce_session_state_from_history_items(
        &session,
        &repaired_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let repaired_active =
        active_work_contract_for_history_items(&session, &repaired_items, &repaired_state, &[]);
    let repaired_ok = repaired_state.process_phase == ProcessPhase::Verify
        && repaired_state.active_targets.is_empty()
        && repaired_state.completion.open_work_count == 0
        && repaired_state.completion.verification_pending
        && matches!(
            repaired_active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );

    initial_ok && repaired_ok
}

pub(crate) fn public_command_contract_continuation_projects_compact_source_repair_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace.join("tool.py").as_std_path(),
        "def main():\n    input()\n\nif __name__ == '__main__':\n    main()\n",
    )
    .is_err()
        || fs::write(
            workspace.join("test_tool.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "public command continuation projection".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Manual ST verification-repair continuation.

Repair targets:
- tool.py

Failed required verification commands:
- python -X utf8 tool.py 2 + 3
- python -X utf8 tool.py 8 +

Latest verification failure evidence:
- command: python -X utf8 tool.py 2 + 3
  requirement_id: public_command_contract
  expected: route-owned public argv command contract passes with the recorded exit code and stdout/stderr observation
  observed: argv invocation entered interactive stdin mode and reached EOF instead of processing command-line arguments
  failure_class: public_command_contract_failed: expected exit 0 but got Some(1); stdout had no line ending with `5`

After the repair edit, rerun the failed required verification command(s) with shell.
"#;
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: continuation.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let cluster = state.verification.failure_cluster.as_ref();
    state.process_phase == ProcessPhase::Repair
        && state.active_targets == vec![Utf8PathBuf::from("tool.py")]
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -X utf8 tool.py 2 + 3")
        && cluster.is_some_and(|cluster| {
            cluster.primary_failure.as_deref().is_some_and(|failure| {
                failure.contains("public_command_contract_failed")
                    && failure.contains("direct argv command handling")
                    && !failure.contains("Traceback")
                    && !failure.contains("C:\\")
            }) && cluster.evidence.iter().any(|evidence| {
                evidence.subtype.as_deref() == Some("public_command_contract_failure")
                    && evidence
                        .observed
                        .as_deref()
                        .is_some_and(|observed| observed.contains("interactive stdin mode"))
            })
        })
}

pub(crate) fn verification_repair_continuation_generated_test_parse_target_fixture_passes() -> bool
{
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::write(
        workspace.join("widget.py").as_std_path(),
        "def render(value):\n    return str(value)\n",
    )
    .is_err()
        || fs::write(
            workspace.join("test_widget.py").as_std_path(),
            "import unittest\n\"\"\"\n",
        )
        .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification repair continuation generated test target".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let test_path = workspace.join("test_widget.py");
    let continuation = format!(
        r#"
Manual ST verification-repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Repair targets:
- test_widget.py

Failed required verification commands:
- python -m unittest

Latest verification failure evidence:
- command: python -m unittest
stderr: E
======================================================================
ERROR: test_widget (unittest.loader._FailedTest.test_widget)
----------------------------------------------------------------------
ImportError: Failed to import test module: test_widget
Traceback (most recent call last):
  File "C:\Python313\Lib\unittest\loader.py", line 396, in _find_test_path
    module = self._get_module_from_name(name)
  File "{test_path}", line 42
    """
    ^
SyntaxError: unterminated triple-quoted string literal (detected at line 42)

Expected artifacts:
- widget.py
- test_widget.py

After the repair edit, rerun the failed required verification command(s) with shell.
"#
    );
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text { text: continuation }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let diagnostic = crate::agent::turn_decision::build_turn_decision_diagnostic(
        &state,
        active.as_ref(),
        &crate::agent::prompt::PromptPolicy::default(),
        &allowed,
        Some("auto".to_string()),
    );

    state.process_phase == ProcessPhase::Repair
        && state.active_targets == vec![Utf8PathBuf::from("test_widget.py")]
        && state
            .verification
            .failure_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence.subtype.as_deref() == Some("source_parse_defect")
                        && evidence.target.as_deref() == Some("test_widget.py")
                        && evidence.source_refs.is_empty()
                })
            })
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("test_widget.py")]
        )
        && diagnostic.active_targets == vec![Utf8PathBuf::from("test_widget.py")]
        && diagnostic.repair_lane.as_ref().is_some_and(|lane| {
            lane.required_target.as_deref() == Some("test_widget.py")
                && lane
                    .repair_control_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| {
                        snapshot.required_target.as_deref() == Some("test_widget.py")
                    })
        })
}

pub(crate) fn requested_work_completion_promotes_verification_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification promotion".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:\\workspace\\project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py; Added test_component.py".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.completion.verification_pending
        && !state.completion.closeout_ready
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
        && requested_work_absolute_docs_file_change_promotes_verification_fixture_passes()
        && requested_work_repair_continuation_expected_artifacts_do_not_reopen_fixture_passes()
        && scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes()
        && invalid_authoring_edit_no_progress_preserves_missing_requested_target_fixture_passes()
        && empty_artifact_tool_output_does_not_satisfy_requested_work_fixture_passes()
}

pub(crate) fn invalid_authoring_edit_no_progress_preserves_missing_requested_target_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let invalid_call_id = ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "invalid edit no progress preserves missing target".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:\\workspace\\project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let malformed_patch = "*** Begin Patch\n*** Add File: source.py\n+def value():\n+    return 1\n*** End Patch\n*** Begin Patch\n*** Add File: test_source.py\n+import unittest\n+\n+class TestValue(unittest.TestCase):\n+    pass\n*** End Patch";
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
                    text: "Create `source.py` and `test_source.py`, then run `python -m unittest`."
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("source.py")),
                    summary: "Added source.py".to_string(),
                }],
                summary: "Added source.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolCall {
                call_id: invalid_call_id,
                tool: ToolName::ApplyPatch,
                arguments: Value::Null,
                model_arguments: json!({ "patch_text": malformed_patch }),
                effective_arguments: json!({ "patch_text": malformed_patch }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: invalid_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Invalid tool arguments".to_string(),
                output_text:
                    "Invalid arguments for `apply_patch`: malformed patch; no side effects applied"
                        .to_string(),
                metadata: json!({
                    "operation_progress_class": "invalid_edit_arguments",
                    "progress_effect": "no_progress",
                    "tool_feedback_envelope": {
                        "kind": "invalid_edit_arguments",
                        "operation_progress_class": "invalid_edit_arguments",
                        "progress_effect": "no_progress",
                        "side_effects_applied": false,
                        "active_targets": ["test_source.py"]
                    }
                }),
                success: Some(false),
                progress_effect: ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-invalid-edit-no-progress".to_string()),
                verification_run: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    matches!(state.process_phase, ProcessPhase::Author)
        && !state.completion.verification_pending
        && state.completion.open_work_count == 1
        && state.active_targets == vec![Utf8PathBuf::from("test_source.py")]
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("test_source.py")]
        )
}

pub(crate) fn empty_artifact_tool_output_does_not_satisfy_requested_work_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let empty_file_call_id = ToolCallId::new();
    let empty_change_id = ChangeId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "empty artifact does not satisfy requested work".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:\\workspace\\project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`."
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Added component.py".to_string(),
                }],
                summary: "Added component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: empty_file_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Create empty file".to_string(),
                output_text: "Length 0 test_component.py".to_string(),
                metadata: json!({
                    "operation_intent": "content_changing_authoring_required",
                    "operation_progress_class": "empty_artifact_no_progress",
                    "progress_effect": "no_progress",
                    "changed_files": [empty_change_id],
                    "file_change_content_evidence": {
                        "kind": "file_change_content_evidence",
                        "content_bearing": false,
                        "all_changes_content_bearing": false,
                        "content_bearing_change_ids": [],
                        "non_satisfying_change_ids": [empty_change_id.to_string()],
                        "content_bearing_paths": [],
                        "non_satisfying_paths": ["test_component.py"]
                    },
                    "tool_feedback_envelope": {
                        "kind": "operation_progress_classification",
                        "operation_intent": "content_changing_authoring_required",
                        "operation_progress_class": "empty_artifact_no_progress",
                        "progress_effect": "no_progress",
                        "side_effects_applied": true,
                        "active_targets": ["test_component.py"]
                    }
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-empty-artifact-no-progress".to_string()),
                verification_run: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Author
        && !state.completion.verification_pending
        && state.active_targets == vec![Utf8PathBuf::from("test_component.py")]
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("test_component.py")]
        )
}

pub(crate) fn scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "scenario contract reference input".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:\\workspace\\project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`\nTreat these files as prompt-visible, harness-owned contract references. Generated tests may assert only the listed requirement ids."
                    .to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let expected_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    let scenario_refs = vec![
        Utf8PathBuf::from("scenario_contract.json"),
        Utf8PathBuf::from("scenario_contract.md"),
    ];

    state.process_phase == ProcessPhase::Author
        && state.active_targets == expected_targets
        && scenario_refs
            .iter()
            .all(|reference| state.contract_refs.iter().any(|item| item == reference))
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                verification_commands
            }) if pending_targets == expected_targets
                && verification_commands == vec!["python -m unittest".to_string()]
        )
}

pub(crate) fn same_document_update_uses_prior_authored_doc_not_contract_ref_fixture_passes() -> bool
{
    let workspace =
        std::env::temp_dir().join(format!("moyai_docs_route_same_doc_{}", ChangeId::new()));
    let Ok(workspace) = Utf8PathBuf::from_path_buf(workspace) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err() {
        return false;
    }
    if fs::write(
        workspace.join("docs/component-design.md").as_std_path(),
        "# Component Design\n\n## Overview\n\nExisting authored design.\n",
    )
    .is_err()
    {
        return false;
    }
    if fs::write(
        workspace.join("scenario_contract.md").as_std_path(),
        "# Scenario Contract\n\nReference only.\n",
    )
    .is_err()
    {
        return false;
    }
    if fs::write(
        workspace.join("scenario_contract.json").as_std_path(),
        "{\"id\":\"scenario_contract.component.v1\"}\n",
    )
    .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "docs route same document update".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `docs/component-design.md` from current implementation.\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`\nTreat these files as prompt-visible, harness-owned contract references."
                        .to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/component-design.md")),
                    summary: "Created docs/component-design.md".to_string(),
                }],
                summary: "Created docs/component-design.md".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn,
            sequence_no: 1,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "いま作成した設計書を、拡張仕様へ更新してください。\nこの turn では文書だけを更新し、実装コードと test はまだ変更しないでください。\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`\nTreat these files as prompt-visible, harness-owned contract references."
                        .to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let expected = vec![Utf8PathBuf::from("docs/component-design.md")];

    state.route == TaskRoute::Docs
        && state.docs_route.is_some()
        && state.process_phase == ProcessPhase::Author
        && state.active_targets == expected
        && state
            .contract_refs
            .iter()
            .any(|target| target == &Utf8PathBuf::from("scenario_contract.md"))
        && !state
            .active_targets
            .iter()
            .any(|target| is_scenario_contract_ref(target.as_str()))
        && matches!(
            active,
            Some(ActiveWorkContract::DocsRepair {
                ref deliverable,
                ref pending_deliverables,
                ..
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/component-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|deliverable| deliverable.target == Utf8PathBuf::from("docs/component-design.md"))
        )
}

pub(crate) fn same_document_update_stays_pending_after_prior_doc_satisfied_fixture_passes() -> bool
{
    let workspace = std::env::temp_dir().join(format!(
        "moyai_docs_route_satisfied_same_doc_{}",
        ChangeId::new()
    ));
    let Ok(workspace) = Utf8PathBuf::from_path_buf(workspace) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err() {
        return false;
    }
    for (path, content) in [
        (
            "component.py",
            "def calculate(left, operator, right):\n    return left + right\n",
        ),
        (
            "test_component.py",
            "import unittest\n\nclass ComponentTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(5, 5)\n",
        ),
        (
            "docs/component-design.md",
            "# Component design\n\n## Overview\n\n現在の実装 `component.py` と test_component.py の tests を確認し、repository evidence、CLI usage、error handling、validation を文書化しています。\n",
        ),
        (
            "scenario_contract.md",
            "# Scenario Contract\n\nReference only.\n",
        ),
        (
            "scenario_contract.json",
            "{\"id\":\"scenario_contract.component.v1\"}\n",
        ),
    ] {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    let session_id = crate::session::SessionId::new();
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "satisfied same-document docs update".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 4,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "現在の実装を調査し、`docs/component-design.md` を日本語で作成してください。実装コードと test は変更せず、確認できた事実だけを文書化してください。".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/component-design.md")),
                    summary: "Created docs/component-design.md".to_string(),
                }],
                summary: "Created docs/component-design.md".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn,
            sequence_no: 1,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "いま作成した設計書を、関数電卓版の仕様へ更新してください。\n四則演算に加えて `sqrt` と `pow` を扱える仕様にしてください。\nこの turn では文書だけを更新し、実装コードと test はまだ変更しないでください。\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);

    state.route == TaskRoute::Docs
        && state.process_phase == ProcessPhase::Author
        && state.completion.route_contract_pending
        && !state.completion.closeout_ready
        && state.active_targets == vec![Utf8PathBuf::from("docs/component-design.md")]
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| {
                reason.contains("same-document docs update requested")
                    && reason.contains("docs/component-design.md")
            })
        && matches!(
            active,
            Some(ActiveWorkContract::DocsRepair {
                ref deliverable,
                ref pending_deliverables,
                route_contract_satisfied: false,
                ..
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/component-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|item| item.target == Utf8PathBuf::from("docs/component-design.md")
                        && item.summary.contains("same-document docs update requested"))
        )
}

pub(crate) fn requested_work_relative_workspace_absolute_file_change_promotes_verification_fixture_passes()
-> bool {
    let Ok(current_dir) = std::env::current_dir() else {
        return false;
    };
    let Ok(current_dir) = Utf8PathBuf::from_path_buf(current_dir) else {
        return false;
    };
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let relative_workspace = Utf8PathBuf::from("../project_sandbox/fr10_018_fixture_workspace");
    let absolute_doc = current_dir
        .join(&relative_workspace)
        .join("docs/tool-design.md");
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "relative workspace absolute file change".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: relative_workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `docs/tool-design.md` from the current implementation. Do not change code or tests. Then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(absolute_doc),
                    summary: "Added docs/tool-design.md".to_string(),
                }],
                summary: "Added docs/tool-design.md".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Verify
        && state.active_targets.is_empty()
        && state.completion.open_work_count == 0
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

fn requested_work_absolute_docs_file_change_promotes_verification_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let workspace = Utf8PathBuf::from("C:\\workspace\\project");
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "absolute docs verification promotion".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `docs/widget-design.md` from the current implementation. Do not change code or tests. Then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Applied 1 change(s)".to_string(),
                output_text: "Added docs/widget-design.md".to_string(),
                metadata: json!({
                    "changes": [{
                        "kind": "add",
                        "path_after": "C:/workspace/project/docs/widget-design.md",
                        "path_before": null
                    }],
                    "changed_files": ["C:/workspace/project/docs/widget-design.md"]
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let authored_state_ok = state.completion.open_work_count == 0
        && state.active_targets.is_empty()
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );
    let mut verified_items = items;
    verified_items.push(HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: ToolCallId::new(),
            status: ToolLifecycleStatus::Completed,
            title: "Run shell command: python -m unittest".to_string(),
            output_text: "Ran 24 tests in 0.000s\n\nOK".to_string(),
            metadata: Value::Null,
            success: Some(true),
            progress_effect: ToolProgressEffect::VerificationPassed,
            blocked_action: None,
            result_hash: Some("verification-pass".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "python -m unittest".to_string(),
                status: VerificationRunStatus::Passed,
                exit_code: Some(0),
                timed_out: false,
                output_summary: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                failure_cluster: None,
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    });
    let verified_state =
        reduce_session_state_from_history_items(&session, &verified_items, &[], &state);
    let escaped_absolute_metadata_ok =
        requested_work_escaped_absolute_docs_file_change_promotes_verification_fixture_passes();
    authored_state_ok
        && verified_state.active_targets.is_empty()
        && verified_state.completion.open_work_count == 0
        && !verified_state.completion.verification_pending
        && verified_state.completion.closeout_ready
        && escaped_absolute_metadata_ok
}

fn requested_work_escaped_absolute_docs_file_change_promotes_verification_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let workspace = Utf8PathBuf::from("C:\\workspace\\project");
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "escaped absolute docs verification promotion".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `docs/widget-design.md` from the current implementation. Then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Applied 1 change(s)".to_string(),
                output_text: "Added docs/widget-design.md".to_string(),
                metadata: json!({
                    "changes": [{
                        "kind": "add",
                        "path_after": r"C:\\workspace\\project\\docs\\widget-design.md",
                        "path_before": null
                    }],
                    "changed_files": [r"C:\\workspace\\project\\docs\\widget-design.md"]
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Verify
        && state.active_targets.is_empty()
        && state.completion.open_work_count == 0
        && state.completion.verification_pending
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

fn requested_work_repair_continuation_expected_artifacts_do_not_reopen_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace.join("widget.py").as_std_path(),
            "def value():\n    return 1\n",
        )
        .is_err()
        || fs::write(
            workspace.join("docs/widget-design.md").as_std_path(),
            "# Widget design\n",
        )
        .is_err()
        || fs::write(
            workspace.join("test_widget.py").as_std_path(),
            "import unittest\n",
        )
        .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "repair continuation expected artifact inventory".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: r#"
Verification-repair continuation.

All required artifacts are present, but the latest required verification command failed.

Repair targets:
- widget.py

Failed required verification commands:
- python -m unittest

Expected artifacts:
- widget.py
- docs/widget-design.md
- test_widget.py

Fix the repair target, then rerun the failed required verification command.
"#
                    .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("widget.py")),
                    path_after: Some(Utf8PathBuf::from("widget.py")),
                    summary: "Updated widget.py".to_string(),
                }],
                summary: "Updated widget.py".to_string(),
            },
        },
    ];

    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let continuation_ok = state.active_targets.is_empty()
        && state.completion.open_work_count == 0
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );

    let normal_docs_targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace.as_path(),
        Some("Create `docs/widget-design.md`, then run `python -m unittest`."),
    );

    continuation_ok && normal_docs_targets == vec![Utf8PathBuf::from("docs/widget-design.md")]
}

pub(crate) fn requested_work_without_verification_closes_after_file_change_fixture_passes() -> bool
{
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "requested deliverable closeout".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Use docling_convert to summarize every docx and xlsx file in this folder into `docs.md`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs.md")),
                    summary: "Added docs.md".to_string(),
                }],
                summary: "Added docs.md".to_string(),
            },
        },
    ];
    let prior_state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("docs.md")],
        completion: CompletionState {
            open_work_count: 1,
            closeout_ready: false,
            verification_pending: false,
            blocked_reason: Some(
                ActiveWorkContract::RequestedWorkAuthoring {
                    pending_targets: vec![Utf8PathBuf::from("docs.md")],
                    verification_commands: Vec::new(),
                }
                .summary(),
            ),
            ..CompletionState::default()
        },
        ..SessionStateSnapshot::default()
    };
    let state = reduce_session_state_from_history_items(&session, &items, &[], &prior_state);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Closeout
        && state.completion.closeout_ready
        && !state.completion.verification_pending
        && state.completion.open_work_count == 0
        && state.completion.blocked_reason.is_none()
        && state.active_targets.is_empty()
        && active.is_none()
}

pub(crate) fn structured_document_summary_waits_for_remaining_sources_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let root = std::env::temp_dir().join(format!("moyai-structured-summary-{}", session_id));
    let _ = fs::remove_dir_all(&root);
    if fs::create_dir_all(&root).is_err()
        || fs::write(root.join("a.docx"), b"a").is_err()
        || fs::write(root.join("b.docx"), b"b").is_err()
        || fs::write(root.join("c.xlsx"), b"c").is_err()
    {
        return false;
    }
    let Ok(cwd) = Utf8PathBuf::from_path_buf(root.clone()) else {
        let _ = fs::remove_dir_all(&root);
        return false;
    };
    let call_id = ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "structured document summary".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let docling_args = serde_json::json!({ "path": "a.docx" });
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
                    text: "Use docling_convert to summarize all docx / xlsx files into `docs.md`. Process 2 files at a time.".to_string(),
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
                tool: ToolName::DoclingConvert,
                arguments: docling_args.clone(),
                model_arguments: docling_args.clone(),
                effective_arguments: docling_args,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: Vec::new(),
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
                title: "Docling converted a.docx".to_string(),
                output_text: "Docling status: success".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs.md")),
                    summary: "Added docs.md".to_string(),
                }],
                summary: "Added docs.md".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let _ = fs::remove_dir_all(&root);
    state.process_phase == ProcessPhase::Author
        && !state.completion.closeout_ready
        && state.active_targets == vec![Utf8PathBuf::from("docs.md")]
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("structured document summary is incomplete"))
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring { .. })
        )
}

pub(crate) fn structured_document_summary_output_headings_survive_compacted_history_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let root =
        std::env::temp_dir().join(format!("moyai-structured-summary-compacted-{}", session_id));
    let _ = fs::remove_dir_all(&root);
    let docs_body = "# Summary\n\n## Batch 1\n\n### a.docx\n\nDone.\n\n### b.docx\n\nDone.\n\n## Batch 2\n\n### c.xlsx\n\nDone.\n";
    if fs::create_dir_all(&root).is_err()
        || fs::write(root.join("a.docx"), b"a").is_err()
        || fs::write(root.join("b.docx"), b"b").is_err()
        || fs::write(root.join("c.xlsx"), b"c").is_err()
        || fs::write(root.join("docs.md"), docs_body).is_err()
    {
        return false;
    }
    let Ok(cwd) = Utf8PathBuf::from_path_buf(root.clone()) else {
        let _ = fs::remove_dir_all(&root);
        return false;
    };
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compacted structured document summary".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Use docling_convert to summarize all docx / xlsx files into `docs.md`."
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("docs.md")),
                    path_after: Some(Utf8PathBuf::from("docs.md")),
                    summary: "Updated docs.md".to_string(),
                }],
                summary: "Updated docs.md".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let _ = fs::remove_dir_all(&root);
    state.process_phase == ProcessPhase::Closeout
        && state.completion.closeout_ready
        && state.completion.blocked_reason.is_none()
        && state.active_targets.is_empty()
        && active.is_none()
}

pub(crate) fn required_verification_survives_authoring_completion_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let first_turn_id = TurnId::new();
    let second_turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification survives authoring completion".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 38,
            created_at_ms: 38,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "Ran 21 tests in 0.000s\n\nOK".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 21 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn_id,
            sequence_no: 1,
            created_at_ms: 100,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "`docs/component-design.md` の拡張仕様に合わせて実装と test を更新してください。\n\n要件:\n- `component.py` に `pow` と `mod` を追加すること。\n- 入力値 validation と error handling を設計書と一致させること。\n- `test_component.py` に追加仕様の unittest を入れること。\n\n最後に `python -m unittest` を実行して成功を確認してから終了してください。".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn_id,
            sequence_no: 36,
            created_at_ms: 136,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("component.py")),
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Updated component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("test_component.py")),
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Updated test_component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("docs/component-design.md")),
                        path_after: Some(Utf8PathBuf::from("docs/component-design.md")),
                        summary: "Updated docs/component-design.md".to_string(),
                    },
                ],
                summary: "Updated component.py, test_component.py, and docs/component-design.md".to_string(),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);

    matches!(state.process_phase, ProcessPhase::Verify)
        && state.completion.verification_pending
        && !state.completion.closeout_ready
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn reference_design_input_does_not_become_pending_authoring_target_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err() {
        return false;
    }
    for (path, content) in [
        ("component.py", "def add(a, b):\n    return a + b\n"),
        (
            "test_component.py",
            "import unittest\n\nclass ComponentTest(unittest.TestCase):\n    pass\n",
        ),
        (
            "docs/component-design.md",
            "# Component design\n\nAdd power and modulo behavior.\n",
        ),
    ] {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "reference design input".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let user_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "`docs/component-design.md` の拡張仕様に合わせて実装と test を更新してください。\n\n要件:\n- `component.py` に `pow` と `mod` を追加すること。\n- 入力値 validation と error handling を設計書と一致させること。\n- `test_component.py` に追加仕様の unittest を入れること。\n\n最後に `python -m unittest` を実行して成功を確認してから終了してください。".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };

    let authoring_items = vec![user_item.clone()];
    let authoring_state = reduce_session_state_from_history_items(
        &session,
        &authoring_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let authoring_active =
        active_work_contract_for_history_items(&session, &authoring_items, &authoring_state, &[]);
    let authoring_targets_are_code_and_test = authoring_state.process_phase == ProcessPhase::Author
        && authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_component.py")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/component-design.md")
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets.iter().any(|target| target.as_str() == "component.py")
                && pending_targets.iter().any(|target| target.as_str() == "test_component.py")
                && !pending_targets.iter().any(|target| target.as_str() == "docs/component-design.md")
        );
    if !authoring_targets_are_code_and_test {
        return false;
    }

    let completed_items = vec![
        user_item,
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("component.py")),
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Updated component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("test_component.py")),
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Updated test_component.py".to_string(),
                    },
                ],
                summary: "Updated component.py and test_component.py".to_string(),
            },
        },
    ];
    let completed_state = reduce_session_state_from_history_items(
        &session,
        &completed_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let completed_active =
        active_work_contract_for_history_items(&session, &completed_items, &completed_state, &[]);
    matches!(completed_state.process_phase, ProcessPhase::Verify)
        && completed_state.completion.verification_pending
        && !completed_state.completion.closeout_ready
        && !completed_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/component-design.md")
        && completed_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            completed_active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn same_document_reference_update_remains_authoring_target_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err() {
        return false;
    }
    for (path, content) in [
        ("component.py", "def add(a, b):\n    return a + b\n"),
        (
            "test_component.py",
            "import unittest\n\nclass ComponentTest(unittest.TestCase):\n    pass\n",
        ),
        (
            "docs/component-design.md",
            "# Component design\n\nCurrent four-operation component.\n",
        ),
    ] {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "same document docs update".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let user_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "前回作成した `docs/component-design.md` をもとに、電卓仕様を拡張してください。\n今回は実装コードと test は変更せず、設計書だけを更新してください。\n\n追加仕様:\n- 累乗 `pow`\n- 剰余 `mod`\n- 入力値 validation\n- CLI 利用例\n- error handling 方針\n\n最後に `python -m unittest` を実行して既存実装が壊れていないことを確認してください。".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };

    let authoring_items = vec![user_item.clone()];
    let authoring_state = reduce_session_state_from_history_items(
        &session,
        &authoring_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let authoring_active =
        active_work_contract_for_history_items(&session, &authoring_items, &authoring_state, &[]);
    let docs_update_is_authoring = authoring_state.process_phase == ProcessPhase::Author
        && authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/component-design.md")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_component.py")
        && authoring_state.route == TaskRoute::Docs
        && authoring_state.docs_route.is_some()
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::DocsRepair {
                ref deliverable,
                ref pending_deliverables,
                ..
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/component-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|deliverable| deliverable.target == Utf8PathBuf::from("docs/component-design.md"))
        );
    if !docs_update_is_authoring {
        return false;
    }

    if fs::write(
        session.cwd.join("docs/component-design.md").as_std_path(),
        "# Component design\n\n## Overview\n\nUpdated docs describe the 実装 files component.py and test_component.py, CLI usage, error handling, validation, pow, and mod behavior for the implementation.\n",
    )
    .is_err()
    {
        return false;
    }

    let completed_items = vec![
        user_item,
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("docs/component-design.md")),
                    path_after: Some(Utf8PathBuf::from("docs/component-design.md")),
                    summary: "Updated docs/component-design.md".to_string(),
                }],
                summary: "Updated docs/component-design.md".to_string(),
            },
        },
    ];
    let completed_state = reduce_session_state_from_history_items(
        &session,
        &completed_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let completed_active =
        active_work_contract_for_history_items(&session, &completed_items, &completed_state, &[]);

    matches!(completed_state.process_phase, ProcessPhase::Verify)
        && completed_state.completion.verification_pending
        && !completed_state.completion.closeout_ready
        && completed_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            completed_active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn japanese_prompt_filename_boundaries_remain_artifact_targets_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    let text = "Pythonで小さな挨拶CLIを作ってください。hello.py と test_hello.py を作成し、python -m unittest で検証してください。仕様: greet(name) は Hello, {name}! を返し、CLI は第一引数の名前に対して挨拶を出力します。";
    let contract = requested_work_contract_from_instruction_text(text);
    if contract.deliverable_targets != vec!["hello.py".to_string(), "test_hello.py".to_string()] {
        return false;
    }
    if !contract
        .verification_commands
        .iter()
        .any(|command| command == "python -m unittest")
    {
        return false;
    }

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "japanese filename boundary".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let user_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };

    let initial_items = vec![user_item.clone()];
    let initial_state = reduce_session_state_from_history_items(
        &session,
        &initial_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let initial_active =
        active_work_contract_for_history_items(&session, &initial_items, &initial_state, &[]);
    let expected_initial_targets = vec![
        Utf8PathBuf::from("hello.py"),
        Utf8PathBuf::from("test_hello.py"),
    ];
    if initial_state.process_phase != ProcessPhase::Author
        || initial_state.active_targets != expected_initial_targets
        || !matches!(
            initial_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == expected_initial_targets
        )
    {
        return false;
    }

    let partial_items = vec![
        user_item.clone(),
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("hello.py")),
                    summary: "Added hello.py".to_string(),
                }],
                summary: "Added hello.py".to_string(),
            },
        },
    ];
    let partial_state = reduce_session_state_from_history_items(
        &session,
        &partial_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    if partial_state.process_phase != ProcessPhase::Author
        || partial_state.active_targets != vec![Utf8PathBuf::from("test_hello.py")]
    {
        return false;
    }

    let completed_items = vec![
        user_item,
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("hello.py")),
                        summary: "Added hello.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_hello.py")),
                        summary: "Added test_hello.py".to_string(),
                    },
                ],
                summary: "Added hello.py and test_hello.py".to_string(),
            },
        },
    ];
    let completed_state = reduce_session_state_from_history_items(
        &session,
        &completed_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let completed_active =
        active_work_contract_for_history_items(&session, &completed_items, &completed_state, &[]);

    matches!(completed_state.process_phase, ProcessPhase::Verify)
        && completed_state.completion.verification_pending
        && !completed_state.completion.closeout_ready
        && completed_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            completed_active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn docs_output_referenced_code_does_not_become_pending_authoring_target_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for (path, content) in [
        (
            "hello.py",
            "def greet(name):\n    return f'Hello, {name}!'\n",
        ),
        (
            "test_hello.py",
            "import unittest\n\nclass HelloTest(unittest.TestCase):\n    pass\n",
        ),
    ] {
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    let text = "この同じセッションで README.md を追加し、hello.py の使い方とテスト実行方法を短く書いてください。最後に python -m unittest を実行して確認してください。";
    let contract = requested_work_contract_from_instruction_text(text);
    if contract.deliverable_targets != vec!["README.md".to_string()]
        || !contract
            .reference_inputs
            .iter()
            .any(|target| target == "hello.py")
        || contract
            .deliverable_targets
            .iter()
            .any(|target| target == "hello.py")
        || !contract
            .verification_commands
            .iter()
            .any(|command| command == "python -m unittest")
    {
        return false;
    }

    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "docs output referenced code".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let user_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: text.to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };

    let initial_items = vec![user_item.clone()];
    let initial_state = reduce_session_state_from_history_items(
        &session,
        &initial_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let initial_active =
        active_work_contract_for_history_items(&session, &initial_items, &initial_state, &[]);
    let expected_initial_targets = vec![Utf8PathBuf::from("README.md")];
    if initial_state.process_phase != ProcessPhase::Author
        || initial_state.active_targets != expected_initial_targets
        || !matches!(
            initial_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == expected_initial_targets
        )
    {
        return false;
    }

    let authored_items = vec![
        user_item.clone(),
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("README.md")),
                    summary: "Added README.md".to_string(),
                }],
                summary: "Added README.md".to_string(),
            },
        },
    ];
    let authored_state = reduce_session_state_from_history_items(
        &session,
        &authored_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let authored_active =
        active_work_contract_for_history_items(&session, &authored_items, &authored_state, &[]);
    if authored_state.process_phase != ProcessPhase::Verify
        || authored_state.completion.closeout_ready
        || !authored_state.completion.verification_pending
        || authored_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "hello.py")
        || !authored_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        || !matches!(
            authored_active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
    {
        return false;
    }

    let verified_items = vec![
        user_item,
        authored_items[1].clone(),
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "Ran 3 tests in 0.000s\n\nOK".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("docs-output-reference-code-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 3 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let verified_state = reduce_session_state_from_history_items(
        &session,
        &verified_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let verified_active =
        active_work_contract_for_history_items(&session, &verified_items, &verified_state, &[]);

    verified_state.process_phase == ProcessPhase::Closeout
        && verified_state.completion.closeout_ready
        && !verified_state.completion.verification_pending
        && verified_state.verification.required_commands.is_empty()
        && verified_active.is_none()
        && verified_state
            .active_targets
            .iter()
            .all(|target| target.as_str() != "hello.py")
}

pub(crate) fn verification_failure_preserves_repair_targets_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification repair target preservation".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let cluster = public_class_attribute_cluster_fixture();
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py; Added test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::SessionState {
                state: verify_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AttributeError: module 'component' has no attribute 'calculate'"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary:
                        "AttributeError: module 'component' has no attribute 'calculate'"
                            .to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && state
            .active_targets
            .iter()
            .all(|target| target.as_str() != "test_component.py")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "10 + 5" || target.as_str() == "5")
}

pub(crate) fn verification_failure_ignores_runtime_loader_frame_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification import repair target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let output_summary = "E\n\
======================================================================\n\
ERROR: test_component (unittest.loader._FailedTest.test_component)\n\
----------------------------------------------------------------------\n\
ImportError: Failed to import test module: test_component\n\
Traceback (most recent call last):\n\
  File \"C:\\Python313\\Lib\\unittest\\loader.py\", line 396, in _find_test_path\n\
    module = self._get_module_from_name(name)\n\
  File \"C:\\Python313\\Lib\\unittest\\loader.py\", line 339, in _get_module_from_name\n\
    __import__(name)\n\
  File \"C:\\workspace\\project\\test_component.py\", line 4, in <module>\n\
    from component import add, subtract, multiply, divide, calculate\n\
ImportError: cannot import name 'calculate' from 'component' (C:\\workspace\\project\\component.py)\n\
\n\
----------------------------------------------------------------------\n\
Ran 1 test in 0.000s\n\
\n\
FAILED (errors=1)\n";
    let evidence = crate::agent::repair_lane::verification_failure_evidence_from_summary(
        FailureKind::VerificationFailed,
        output_summary,
    );
    let source_refs = evidence
        .iter()
        .flat_map(|item| item.source_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let test_refs = evidence
        .iter()
        .flat_map(|item| item.test_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-import-export-runtime-loader-frame".to_string(),
        failing_labels: vec!["test_component".to_string()],
        primary_failure: Some("E".to_string()),
        evidence,
        sibling_obligations: Vec::new(),
        source_refs,
        test_refs,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py; Added test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::SessionState {
                state: verify_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: output_summary.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: output_summary.to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .verification
            .failure_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster
                    .source_refs
                    .iter()
                    .any(|target| target == "component.py")
                    && !cluster
                        .source_refs
                        .iter()
                        .any(|target| target == "loader.py")
            })
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && state
            .active_targets
            .iter()
            .all(|target| target.as_str() != "test_component.py")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "loader.py")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "component.py")
                && !targets.iter().any(|target| target.as_str() == "loader.py")
        )
}

pub(crate) fn out_of_order_history_items_use_sequence_authority_for_repair_fixture_passes() -> bool
{
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "sequence authority repair".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-public-missing-near-name".to_string(),
        failing_labels: vec!["test_float_result".to_string()],
        primary_failure: Some("E".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("test_float_result".to_string()),
            target: Some("test_component.py".to_string()),
            symbol: Some("component._format_result".to_string()),
            call_site: Some("component._format_result(1.5)".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("1.5".to_string()),
            observed: Some("component._format_result missing".to_string()),
            public_state_assertions: vec!["component._format_result(1.5)".to_string()],
            public_missing_attributes: vec!["component._format_result".to_string()],
            evidence_markers: vec![
                "`component._format_result` is missing; source near-name candidate is `component.format_result`".to_string(),
                "public missing method `component._format_result`".to_string(),
            ],
            sibling_obligations: vec!["component._format_result".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_component.py".to_string()],
        }],
        sibling_obligations: vec!["component._format_result".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_component.py".to_string()],
    };
    let old_failure = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: ToolCallId::new(),
            status: ToolLifecycleStatus::Completed,
            title: "Run shell command: python -m unittest".to_string(),
            output_text: "older verification failure".to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            result_hash: Some("old-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "python -m unittest".to_string(),
                status: VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary: "older verification failure".to_string(),
                failure_cluster: Some(cluster.clone()),
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    };
    let initial_authoring_edit = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::FileChange {
            change_ids: vec![ChangeId::new(), ChangeId::new()],
            changes: vec![
                FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("component.py")),
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Updated component.py".to_string(),
                },
                FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("test_component.py")),
                    path_after: Some(Utf8PathBuf::from("test_component.py")),
                    summary: "Updated test_component.py".to_string(),
                },
            ],
            summary: "Updated component.py and test_component.py".to_string(),
        },
    };
    let post_old_repair_edit = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::FileChange {
            change_ids: vec![ChangeId::new()],
            changes: vec![FileChangeEvidence {
                change_id: ChangeId::new(),
                kind: crate::session::ChangeKind::Update,
                path_before: Some(Utf8PathBuf::from("component.py")),
                path_after: Some(Utf8PathBuf::from("component.py")),
                summary: "Edited component.py after an older failure".to_string(),
            }],
            summary: "Edited component.py".to_string(),
        },
    };
    let latest_failure = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 6,
        created_at_ms: 6,
        payload: HistoryItemPayload::ToolOutput {
            call_id: ToolCallId::new(),
            status: ToolLifecycleStatus::Completed,
            title: "Run shell command: python -m unittest".to_string(),
            output_text: "AttributeError: module 'component' has no attribute '_format_result'. Did you mean: 'format_result'?".to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            result_hash: Some("latest-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "python -m unittest".to_string(),
                status: VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary: "AttributeError: module 'component' has no attribute '_format_result'. Did you mean: 'format_result'?".to_string(),
                failure_cluster: Some(cluster),
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    };
    let user = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Update component.py and test_component.py, then run python -m unittest."
                    .to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };
    let items = vec![
        user,
        latest_failure,
        old_failure,
        post_old_repair_edit,
        initial_authoring_edit,
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active_work = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(
        &state,
        &BTreeSet::from([
            "apply_patch".to_string(),
            "shell".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ]),
    );

    let passes = matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && matches!(
            active_work,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ..
            })
        )
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.operation_template.as_ref())
            .and_then(|template| template.exact_target.as_deref())
            == Some("component.py");
    passes
}

pub(crate) fn source_owned_verification_failure_preserves_recent_source_edit_target_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "source-owned repair preserves recent source edit target".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![Utf8PathBuf::from("arcade_game.py")];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-public-state-generated-test-only".to_string(),
        failing_labels: vec![
            "test_beh3_rects_overlap_edge_contact".to_string(),
            "test_beh4_bullet_overlaps_invader_consumes_bullet".to_string(),
        ],
        primary_failure: Some(".....F..F............................".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_beh3_rects_overlap_edge_contact".to_string()),
            target: Some("test_arcade_game.py".to_string()),
            symbol: Some("arcade_game.rects_overlap".to_string()),
            call_site: Some("arcade_game.rects_overlap(a, b)".to_string()),
            exception: None,
            expected: Some("truthy".to_string()),
            observed: Some("False".to_string()),
            public_state_assertions: vec![
                "arcade_game.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "arcade_game.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            sibling_obligations: vec![
                "arcade_game.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            requirement_refs: vec!["BEH-3".to_string(), "BEH-4".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["test_arcade_game.py".to_string()],
        }],
        sibling_obligations: vec![
            "arcade_game.rects_overlap(a, b)".to_string(),
            "len(gs.player_bullets)".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["test_arcade_game.py".to_string()],
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
                    text: "Create `arcade_game.py` and `test_arcade_game.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("arcade_game.py")),
                        path_after: Some(Utf8PathBuf::from("arcade_game.py")),
                        summary: "Updated arcade_game.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_arcade_game.py")),
                        summary: "Added test_arcade_game.py".to_string(),
                    },
                ],
                summary: "Updated arcade_game.py; Added test_arcade_game.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::SessionState {
                state: verify_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "FAIL: test_beh3_rects_overlap_edge_contact\nAssertionError: False is not true\nFAIL: test_beh4_bullet_overlaps_invader_consumes_bullet\nAssertionError: 1 != 0".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-public-state-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: test_beh3_rects_overlap_edge_contact\nAssertionError: False is not true\nFAIL: test_beh4_bullet_overlaps_invader_consumes_bullet\nAssertionError: 1 != 0".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: vec!["BEH-3".to_string(), "BEH-4".to_string()],
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .active_targets
            .first()
            .is_some_and(|target| target.as_str() == "arcade_game.py")
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "arcade_game.py")
}

pub(crate) fn verification_timeout_preserves_recent_source_repair_target_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification timeout source target preservation".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `widget.py` and `test_widget.py`, then run `python -X utf8 -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("widget.py")),
                        summary: "Added widget.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_widget.py")),
                        summary: "Added test_widget.py".to_string(),
                    },
                ],
                summary: "Added widget.py and test_widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -X utf8 -m unittest".to_string(),
                output_text: "command timed out".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::Blocked,
                blocked_action: None,
                result_hash: Some("timeout-fixture".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -m unittest".to_string(),
                    status: VerificationRunStatus::TimedOut,
                    exit_code: Some(1),
                    timed_out: true,
                    output_summary: "command timed out".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active_work = active_work_contract_for_history_items(&session, &items, &state, &[]);
    state.process_phase == ProcessPhase::Repair
        && state.completion.verification_pending
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "widget.py")
        && matches!(
            active_work,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "widget.py")
        )
}

pub(crate) fn verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification labels remain evidence".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let label_target = "BEH-4: bullet overlap assertion message";
    let mut stale_verify_state = SessionStateSnapshot::default();
    stale_verify_state.process_phase = ProcessPhase::Verify;
    stale_verify_state.active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("test_arcade_game.py"),
    ];
    stale_verify_state.completion.verification_pending = true;
    stale_verify_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-diagnostic-label-target-pollution".to_string(),
        failing_labels: vec!["test_update_calls_collision_BEH4".to_string()],
        primary_failure: Some("....F...................F....................".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_update_calls_collision_BEH4".to_string()),
            target: Some(label_target.to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("score increments".to_string()),
            observed: Some("0".to_string()),
            public_state_assertions: vec!["gs.score".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "gs.score".to_string(),
            ],
            sibling_obligations: vec!["gs.score".to_string()],
            requirement_refs: vec!["BEH-4".to_string()],
            source_refs: vec![label_target.to_string()],
            test_refs: vec!["test_arcade_game.py".to_string()],
        }],
        sibling_obligations: vec!["gs.score".to_string()],
        source_refs: vec![label_target.to_string()],
        test_refs: vec!["test_arcade_game.py".to_string()],
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
                    text: "Create arcade_game.py and generated tests.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("arcade_game.py")),
                    path_after: Some(Utf8PathBuf::from("arcade_game.py")),
                    summary: "Updated arcade_game.py".to_string(),
                }],
                summary: "Updated arcade_game.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("test_arcade_game.py")),
                    path_after: Some(Utf8PathBuf::from("test_arcade_game.py")),
                    summary: "Updated test_arcade_game.py".to_string(),
                }],
                summary: "Updated test_arcade_game.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::SessionState {
                state: stale_verify_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "FAIL: test_update_calls_collision_BEH4\nAssertionError: 0 != 40 : BEH-4: bullet overlap assertion message".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-diagnostic-label-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: test_update_calls_collision_BEH4\nAssertionError: 0 != 40 : BEH-4: bullet overlap assertion message".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: vec!["BEH-4".to_string()],
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .active_targets
            .first()
            .is_some_and(|target| target.as_str() == "arcade_game.py")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str().contains("BEH-4:"))
}

pub(crate) fn synthetic_tool_feedback_preserves_real_verification_cluster_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "synthetic feedback preservation".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::SessionState {
                state: verify_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AttributeError: module 'component' has no attribute 'calculate'"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary:
                        "AttributeError: module 'component' has no attribute 'calculate'"
                            .to_string(),
                    failure_cluster: Some(public_class_attribute_cluster_fixture()),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Corrective tool feedback".to_string(),
                output_text: "The requested shell command is not executable in the current tool lifecycle. Preserve the existing verification failure and follow the current active work.".to_string(),
                metadata: serde_json::json!({
                    "tool_name": "shell",
                    "progress_effect": "no_progress"
                }),
                success: Some(false),
                progress_effect: ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("duplicate-feedback".to_string()),
                verification_run: None,
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    state
        .verification
        .failure_cluster
        .as_ref()
        .is_some_and(|cluster| cluster.cluster_id == "fixture-public-class-attribute")
        && state
            .failure
            .as_ref()
            .is_some_and(|failure| failure.summary.contains("component"))
        && !state
            .verification
            .last_evidence_summary
            .as_deref()
            .unwrap_or_default()
            .contains("not the current executable action")
}

pub(crate) fn post_repair_file_change_promotes_verification_rerun_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "post repair verification rerun".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut stale_repair_state = SessionStateSnapshot::default();
    stale_repair_state.process_phase = ProcessPhase::Repair;
    stale_repair_state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    stale_repair_state.completion.verification_pending = true;
    stale_repair_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    stale_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: component.calculate returns stale values".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: stale_repair_state.active_targets.clone(),
    });
    stale_repair_state.verification.failure_cluster =
        Some(public_class_attribute_cluster_fixture());

    let cluster = public_class_attribute_cluster_fixture();
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py; Added test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AssertionError: '15' != 15".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: '15' != 15".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("component.py")),
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Updated component.py".to_string(),
                }],
                summary: "Updated component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::SessionState {
                state: stale_repair_state,
            },
        },
    ];

    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let baseline_source_target_progress = matches!(state.process_phase, ProcessPhase::Verify)
        && state.completion.verification_pending
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );

    let widget_session_id = crate::session::SessionId::new();
    let widget_turn_id = TurnId::new();
    let widget_session = SessionRecord {
        id: widget_session_id,
        project_id: crate::session::ProjectId::new(),
        title: "source-owned generated-test repair progress".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut widget_repair_state = SessionStateSnapshot::default();
    widget_repair_state.process_phase = ProcessPhase::Repair;
    widget_repair_state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    widget_repair_state.completion.verification_pending = true;
    widget_repair_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    widget_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public widget CLI behavior mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: widget_repair_state.active_targets.clone(),
    });
    widget_repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-owned-test-only-progress".to_string(),
        failing_labels: vec!["test_cli_public_behavior".to_string()],
        primary_failure: Some("stdout did not match public command contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_cli_public_behavior".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("public CLI output".to_string()),
            observed: Some("incorrect stderr/stdout split".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["error output".to_string(), "usage text".to_string()],
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["error output".to_string(), "usage text".to_string()],
        test_refs: vec!["test_widget.py".to_string()],
    });
    let widget_items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: widget_session_id,
            turn_id: widget_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `widget.py` and `test_widget.py`, then run `python -m unittest`."
                        .to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: widget_session_id,
            turn_id: widget_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("widget.py")),
                        summary: "Added widget.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_widget.py")),
                        summary: "Added test_widget.py".to_string(),
                    },
                ],
                summary: "Added widget.py and test_widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: widget_session_id,
            turn_id: widget_turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AssertionError: public CLI behavior mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("test-only-failure".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: public CLI behavior mismatch".to_string(),
                    failure_cluster: widget_repair_state.verification.failure_cluster.clone(),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: widget_session_id,
            turn_id: widget_turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("C:\\workspace\\project\\widget.py")),
                    path_after: Some(Utf8PathBuf::from("C:\\workspace\\project\\widget.py")),
                    summary: "Updated widget.py".to_string(),
                }],
                summary: "Updated widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: widget_session_id,
            turn_id: widget_turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::SessionState {
                state: widget_repair_state,
            },
        },
    ];
    let widget_state = reduce_session_state_from_history_items(
        &widget_session,
        &widget_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let widget_active =
        active_work_contract_for_history_items(&widget_session, &widget_items, &widget_state, &[]);
    let source_owned_test_only_progress =
        matches!(widget_state.process_phase, ProcessPhase::Verify)
            && widget_state.completion.verification_pending
            && !widget_state
                .active_targets
                .iter()
                .any(|target| target.as_str() == "test_widget.py")
            && !widget_state
                .active_targets
                .iter()
                .any(|target| target.as_str() == "widget.py")
            && matches!(
                widget_active,
                Some(ActiveWorkContract::Verification {
                    repair_required: false,
                    ..
                })
            );

    let mixed_session_id = crate::session::SessionId::new();
    let mixed_turn_id = TurnId::new();
    let mixed_session = SessionRecord {
        id: mixed_session_id,
        project_id: crate::session::ProjectId::new(),
        title: "mixed active target repair progress".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut mixed_repair_state = SessionStateSnapshot::default();
    mixed_repair_state.process_phase = ProcessPhase::Repair;
    mixed_repair_state.active_targets = vec![
        Utf8PathBuf::from("test_widget.py"),
        Utf8PathBuf::from("widget.py"),
    ];
    mixed_repair_state.completion.verification_pending = true;
    mixed_repair_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    mixed_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test observed public CLI mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    });
    mixed_repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-mixed-active-target-progress".to_string(),
        failing_labels: vec!["test_cli_invalid_args".to_string()],
        primary_failure: Some("stderr did not contain expected usage text".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_cli_invalid_args".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("usage text on stderr".to_string()),
            observed: Some("usage text on stdout".to_string()),
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
    let mixed_items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: mixed_session_id,
            turn_id: mixed_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `widget.py` and `test_widget.py`, then run `python -m unittest`."
                        .to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: mixed_session_id,
            turn_id: mixed_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AssertionError: usage text on stderr".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("mixed-failure".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: usage text on stderr".to_string(),
                    failure_cluster: mixed_repair_state.verification.failure_cluster.clone(),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: mixed_session_id,
            turn_id: mixed_turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Updated widget.py".to_string(),
                output_text: "Updated widget.py".to_string(),
                metadata: serde_json::json!({
                    "changes": [{
                        "kind": "update",
                        "path_before": "C:/workspace/project/widget.py",
                        "path_after": "C:/workspace/project/widget.py"
                    }]
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("mixed-source-progress".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: mixed_session_id,
            turn_id: mixed_turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::SessionState {
                state: mixed_repair_state,
            },
        },
    ];
    let mixed_state = reduce_session_state_from_history_items(
        &mixed_session,
        &mixed_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let mixed_active =
        active_work_contract_for_history_items(&mixed_session, &mixed_items, &mixed_state, &[]);
    let mixed_active_target_source_progress =
        matches!(mixed_state.process_phase, ProcessPhase::Verify)
            && mixed_state.completion.verification_pending
            && matches!(
                mixed_active,
                Some(ActiveWorkContract::Verification {
                    repair_required: false,
                    ..
                })
            );

    baseline_source_target_progress
        && source_owned_test_only_progress
        && mixed_active_target_source_progress
}

pub(crate) fn post_repair_generated_test_public_output_overreach_enters_test_repair_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "post repair generated test output overreach".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let output = r#"FAIL: test_valid_addition (test_widget.TestCliSubprocess.test_valid_addition)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\project\test_widget.py", line 20, in test_valid_addition
    self.assertEqual(result.stdout.strip(), "= 8")
AssertionError: '8' != '= 8'
- 8
+ = 8

----------------------------------------------------------------------
Ran 12 tests in 0.120s

FAILED (failures=1)"#;
    let evidence = crate::agent::repair_lane::verification_failure_evidence_from_summary(
        FailureKind::VerificationFailed,
        output,
    );
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-public-output-overreach".to_string(),
        failing_labels: vec!["test_valid_addition".to_string()],
        primary_failure: Some(output.to_string()),
        evidence,
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
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
                    text: "Create `widget.py` and `test_widget.py`, then run `python -m unittest`."
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("widget.py")),
                        summary: "Added widget.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_widget.py")),
                        summary: "Added test_widget.py".to_string(),
                    },
                ],
                summary: "Added widget.py and test_widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -X utf8 -m unittest".to_string(),
                output_text: output.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("generated-test-output-overreach".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: output.to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let has_generated_test_overreach_marker = state
        .verification
        .failure_cluster
        .as_ref()
        .is_some_and(|cluster| {
            cluster.evidence.iter().any(|evidence| {
                evidence
                    .evidence_markers
                    .iter()
                    .any(|marker| marker == "generated_test_contract_overreach")
            })
        });

    matches!(state.process_phase, ProcessPhase::Repair)
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_widget.py")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "widget.py")
        && has_generated_test_overreach_marker
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "test_widget.py")
        )
}

pub(crate) fn passed_verification_consumes_pending_required_commands_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "passed verification consumes pending commands".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("python -m unittest".to_string());

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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py and test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let diagnostic = crate::agent::turn_decision::build_turn_decision_diagnostic(
        &state,
        active.as_ref(),
        &crate::agent::prompt::PromptPolicy::default(),
        &BTreeSet::from(["shell".to_string(), "write".to_string()]),
        Some("none".to_string()),
    );

    matches!(state.process_phase, ProcessPhase::Closeout)
        && state.completion.closeout_ready
        && !state.completion.verification_pending
        && state.verification.required_commands.is_empty()
        && active.is_none()
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.code != "closeout_ready_with_verification_pending")
}

pub(crate) fn corrected_verification_command_consumes_original_obligation_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "corrected verification command alias".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let corrected =
        "chcp 65001 >$null; $env:PYTHONIOENCODING=\"utf-8\"; python -X utf8 -m unittest";
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
                    text: "Run `python -m unittest` after authoring.".to_string(),
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
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run corrected verification command".to_string(),
                output_text: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("corrected-verification-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: corrected.to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec!["python -m unittest".to_string()],
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("__pycache__/tool.cpython-313.pyc")),
                    path_after: Some(Utf8PathBuf::from("__pycache__/tool.cpython-313.pyc")),
                    summary: "Updated __pycache__/tool.cpython-313.pyc".to_string(),
                }],
                summary: "Updated __pycache__/tool.cpython-313.pyc".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run corrected verification command with runner byproducts".to_string(),
                output_text: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                metadata: json!({
                    "changed_files": [ChangeId::new().to_string()],
                    "tool_result_metadata": {
                        "changed_files": [ChangeId::new().to_string()]
                    }
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("corrected-verification-pass-with-byproducts".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: corrected.to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec!["python -m unittest".to_string()],
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    matches!(state.process_phase, ProcessPhase::Closeout)
        && state.completion.closeout_ready
        && !state.completion.verification_pending
        && state.verification.required_commands.is_empty()
        && active.is_none()
}

pub(crate) fn resumed_new_user_turn_ignores_prior_closeout_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let first_turn_id = TurnId::new();
    let second_turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "resume new user turn".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Closeout;
    previous.completion.closeout_ready = true;

    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `component.py`, then run `python -m unittest`.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("component.py")),
                    summary: "Added component.py".to_string(),
                }],
                summary: "Added component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "OK".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "OK".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `README.md` for the component app.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    matches!(state.process_phase, ProcessPhase::Author)
        && !state.completion.closeout_ready
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "README.md")
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring { .. })
        )
}

pub(crate) fn new_authoring_turn_overrides_prior_verification_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let first_turn_id = TurnId::new();
    let second_turn_id = TurnId::new();
    let temp_root = std::env::temp_dir().join(format!("moyai-fr10-006-{session_id}"));
    if fs::create_dir_all(&temp_root).is_err() {
        return false;
    }
    let _cleanup = TempDirCleanup(temp_root.clone());
    if fs::write(
        temp_root.join("component.py"),
        "def calculate(a, op, b):\n    return a\n",
    )
    .is_err()
        || fs::write(
            temp_root.join("test_component.py"),
            "import unittest\n\nclass ComponentTest(unittest.TestCase):\n    pass\n",
        )
        .is_err()
    {
        return false;
    }
    let Ok(cwd) = Utf8PathBuf::from_path_buf(temp_root) else {
        return false;
    };
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "new authoring after verification".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd,
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("python -m unittest".to_string());

    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: first_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py and test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Update `component.py` and `test_component.py` to support sqrt and pow, then run `python -m unittest`.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let pending_targets = match active {
        Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        }) => pending_targets,
        _ => Vec::new(),
    };
    matches!(state.process_phase, ProcessPhase::Author)
        && !state.completion.verification_pending
        && !state.completion.closeout_ready
        && pending_targets
            .iter()
            .any(|target| target.as_str() == "component.py")
        && pending_targets
            .iter()
            .any(|target| target.as_str() == "test_component.py")
}

struct TempDirCleanup(std::path::PathBuf);

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn partial_verification_pass_preserves_remaining_required_commands_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "partial verification preserves remaining commands".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("python -m py_compile app.py".to_string());
    previous
        .verification
        .required_commands
        .push("python -m unittest".to_string());

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
                    text: "Create `app.py` and `test_app.py`, then run `python -m py_compile app.py` and `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("app.py")),
                        summary: "Added app.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_app.py")),
                        summary: "Added test_app.py".to_string(),
                    },
                ],
                summary: "Added app.py and test_app.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m py_compile app.py".to_string(),
                output_text: String::new(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("partial-passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m py_compile app.py".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: String::new(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    matches!(state.process_phase, ProcessPhase::Verify)
        && !state.completion.closeout_ready
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "python -m unittest")
        && state
            .verification
            .required_commands
            .iter()
            .all(|command| command != "python -m py_compile app.py")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn verification_failure_promotes_repair_required_active_work_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "verification failure repair authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = public_class_attribute_cluster_fixture();
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("component.py")),
                        summary: "Added component.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_component.py")),
                        summary: "Added test_component.py".to_string(),
                    },
                ],
                summary: "Added component.py; Added test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -m unittest".to_string(),
                output_text: "AttributeError: component.calculate is missing".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AttributeError: component.calculate is missing".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];

    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);

    matches!(state.process_phase, ProcessPhase::Repair)
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.first().is_some_and(|target| target.as_str() == "component.py")
        )
}

pub(crate) fn source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "source-owned repair active work target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("component.py"),
        Utf8PathBuf::from("test_component.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public CLI usage error returned exit code 0".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec![
        "test_cli_incomplete_binary".to_string(),
        "test_cli_unknown_function".to_string(),
    ];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-owned-public-cli-exit-code".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("AssertionError: 0 != 1".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_cli_incomplete_binary".to_string()),
            target: Some("8 +".to_string()),
            symbol: None,
            call_site: Some(
                "subprocess.run([sys.executable, \"component.py\", \"8 +\"])".to_string(),
            ),
            exception: None,
            expected: Some("returncode 1".to_string()),
            observed: Some("returncode 0".to_string()),
            public_state_assertions: vec!["result.returncode".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "`result.returncode` expected `1`".to_string(),
                "public_state_assertion_mismatch".to_string(),
                "result.returncode".to_string(),
            ],
            sibling_obligations: vec!["result.returncode".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec!["8 +".to_string(), "log 10".to_string()],
            test_refs: vec!["test_component.py".to_string()],
        }],
        sibling_obligations: vec!["result.returncode".to_string()],
        source_refs: vec!["8 +".to_string(), "log 10".to_string()],
        test_refs: vec!["test_component.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("component.py")]
    )
}

pub(crate) fn source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "source-owned requirement repair target alignment".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-source-owned-requirement-test-ref-only".to_string(),
        failing_labels: vec![
            "test_cli_addition".to_string(),
            "test_cli_invalid_input".to_string(),
        ],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_cli_addition".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: Some("subprocess.TimeoutExpired".to_string()),
            expected: Some("bounded CLI command returns".to_string()),
            observed: Some("child command timed out".to_string()),
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
                    text: "Create `widget.py` and `test_widget.py`, then run `python -m unittest`."
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("widget.py")),
                        summary: "Added widget.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_widget.py")),
                        summary: "Added test_widget.py".to_string(),
                    },
                ],
                summary: "Added widget.py; Added test_widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -X utf8 -m unittest".to_string(),
                output_text: "subprocess.TimeoutExpired in generated CLI tests".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("source-owned-requirement-timeout".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "subprocess.TimeoutExpired in generated CLI tests".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: vec!["API-5".to_string(), "BEH-5".to_string()],
                }),
            },
        },
    ];
    let mut previous = SessionStateSnapshot::default();
    previous.contract_refs = vec![
        Utf8PathBuf::from("scenario_contract.md"),
        Utf8PathBuf::from("scenario_contract.json"),
    ];
    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let diagnostic = crate::agent::turn_decision::build_turn_decision_diagnostic(
        &state,
        active.as_ref(),
        &crate::agent::prompt::PromptPolicy::default(),
        &allowed,
        Some("auto".to_string()),
    );

    matches!(state.process_phase, ProcessPhase::Repair)
        && state.active_targets == vec![Utf8PathBuf::from("widget.py")]
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("widget.py")]
        )
        && diagnostic
            .repair_lane
            .as_ref()
            .is_some_and(|lane| lane.required_target.as_deref() == Some("widget.py"))
        && diagnostic
            .warnings
            .iter()
            .all(|warning| warning.severity != crate::session::TurnDecisionWarningSeverity::Error)
}

pub(crate) fn contract_visible_public_exception_active_work_targets_source_fixture_passes() -> bool
{
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "contract-visible public exception active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
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
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public exception was not raised".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_invalid_public_input".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-contract-visible-public-exception-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("AssertionError: ValueError not raised".to_string()),
        evidence: crate::agent::repair_lane::verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("widget.py")]
    ) && repair_lane.as_ref().is_some_and(|lane| {
        lane.required_target.as_deref() == Some("widget.py")
            && lane
                .contract_reconciliation
                .as_ref()
                .is_some_and(|decision| {
                    decision.owner == "SourceViolatesContract"
                        && decision.source_repair_allowed
                        && !decision.test_repair_allowed
                })
    })
}

pub(crate) fn generated_test_validity_active_work_outranks_source_sibling_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test validity active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: generated test has unresolved helper and widget output mismatch"
                .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec![
        "test_generated_helper_executes".to_string(),
        "test_public_output".to_string(),
    ];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-validity-source-sibling".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_public_output".to_string()),
            target: Some("public-output".to_string()),
            symbol: None,
            call_site: Some("widget.render_public_value()".to_string()),
            exception: Some("NameError".to_string()),
            expected: Some("visible output contract".to_string()),
            observed: Some("NameError: name 'helper_value' is not defined".to_string()),
            public_state_assertions: vec!["widget.render_public_value()".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated test missing name `helper_value`".to_string(),
                "generated test name-resolution frame `test_widget.py`".to_string(),
                "generated_test_artifact_name_resolution_defect".to_string(),
                "public_state_assertion_mismatch".to_string(),
            ],
            sibling_obligations: vec![
                "widget.render_public_value()".to_string(),
                "generated test executable validity".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: vec!["public-output".to_string()],
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec![
            "widget.render_public_value()".to_string(),
            "generated test executable validity".to_string(),
        ],
        source_refs: vec!["public-output".to_string()],
        test_refs: vec!["test_widget.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("test_widget.py")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("test_widget.py")
        })
}

pub(crate) fn generated_test_api_misuse_active_work_targets_test_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test api misuse active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test passed str to inspect.getsource".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_main_guard".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-api-misuse".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("test_main_guard".to_string()),
            target: Some("test_calculator.py".to_string()),
            symbol: Some("inspect.getsource".to_string()),
            call_site: Some("source = inspect.getsource(main.__module__)".to_string()),
            exception: Some("TypeError: code object was expected, got str".to_string()),
            expected: None,
            observed: Some(
                "generated test invalid reflection subject `inspect.getsource(__module__ string)`"
                    .to_string(),
            ),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_artifact_api_misuse".to_string(),
                "generated test invalid reflection subject `inspect.getsource(__module__ string)`"
                    .to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["API-5".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["test_calculator.py".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("test_calculator.py")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("test_calculator.py")
        })
}

pub(crate) fn generated_test_module_attribute_api_misuse_active_work_targets_test_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test module attribute api misuse active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let summary = r#"ERROR: test_cli_addition (test_calculator.TestCalculatorCli.test_cli_addition)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "C:\workspace\test_calculator.py", line 151, in test_cli_addition
    proc = self._run_calculator("3 + 4\n")
  File "C:\workspace\test_calculator.py", line 134, in _run_calculator
    env = dict(sys.environ)
AttributeError: module 'sys' has no attribute 'environ'

----------------------------------------------------------------------
Ran 21 tests in 0.003s

FAILED (errors=1)"#;
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test used invalid module attribute".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_cli_addition".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-module-attribute-api-misuse-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: python -m unittest".to_string()),
        evidence: crate::agent::repair_lane::verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            summary,
        ),
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("test_calculator.py")]
    ) && state
        .verification
        .failure_cluster
        .as_ref()
        .is_some_and(|cluster| {
            cluster.evidence.iter().any(|evidence| {
                evidence.subtype.as_deref() == Some("generated_test_artifact_api_misuse")
                    && evidence.public_missing_attributes.is_empty()
                    && evidence
                        .evidence_markers
                        .iter()
                        .any(|marker| marker == "generated_test_artifact_api_misuse")
            })
        })
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner == "generated_test"
                    && snapshot.required_target.as_deref() == Some("test_calculator.py")
            })
}

pub(crate) fn generated_test_exception_type_overreach_active_work_targets_test_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test exception type overreach active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: generated test over-specified division-by-zero exception type"
                .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_divide_by_zero".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-exception-type-overreach".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("ValueError: division by zero".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("test_divide_by_zero".to_string()),
            target: Some("calculator.py".to_string()),
            symbol: None,
            call_site: Some("divide(10, 0)".to_string()),
            exception: Some("ValueError".to_string()),
            expected: Some("ZeroDivisionError".to_string()),
            observed: Some("ValueError".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_exception_mismatch".to_string(),
                "generated_test_contract_overreach".to_string(),
                "generated-test exception type assertion overreach".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["calculator.py".to_string()],
            test_refs: vec!["test_calculator.py".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["calculator.py".to_string()],
        test_refs: vec!["test_calculator.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("test_calculator.py")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("test_calculator.py")
        })
}

pub(crate) fn mixed_source_public_api_and_generated_test_name_resolution_active_work_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "mixed source/test repair authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: widget public API missing and generated test helper unresolved"
                .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-mixed-source-public-api-generated-test-name-resolution".to_string(),
        failing_labels: vec!["test_widget_api".to_string(), "test_cli_widget".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("public_class_attribute_mismatch".to_string()),
                label: Some("test_widget_api".to_string()),
                target: None,
                symbol: Some("widget.calculate_binary".to_string()),
                call_site: Some("widget.calculate_binary(2, '+', 3)".to_string()),
                exception: Some(
                    "AttributeError: module 'widget' has no attribute 'calculate_binary'"
                        .to_string(),
                ),
                expected: Some("5".to_string()),
                observed: Some("widget.calculate_binary is missing".to_string()),
                public_state_assertions: vec!["widget.calculate_binary(2, '+', 3)".to_string()],
                public_missing_attributes: vec!["widget.calculate_binary".to_string()],
                evidence_markers: vec![
                    "public_class_attribute_mismatch".to_string(),
                    "public missing method `widget.calculate_binary`".to_string(),
                ],
                sibling_obligations: vec!["`widget.calculate_binary` is missing".to_string()],
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["test_widget.py".to_string()],
            },
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("generic_verification_failure".to_string()),
                label: Some("test_cli_widget".to_string()),
                target: Some("test_widget.py".to_string()),
                symbol: Some("envlib".to_string()),
                call_site: Some("env = dict(envlib.environ)".to_string()),
                exception: Some("NameError: name 'envlib' is not defined".to_string()),
                expected: None,
                observed: Some("missing generated-test helper name `envlib`".to_string()),
                public_state_assertions: Vec::new(),
                public_missing_attributes: Vec::new(),
                evidence_markers: vec![
                    "generated test helper unresolved name".to_string(),
                    "generated_test_artifact_name_resolution_defect".to_string(),
                ],
                sibling_obligations: Vec::new(),
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["test_widget.py".to_string()],
            },
        ],
        sibling_obligations: vec!["`widget.calculate_binary` is missing".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("widget.py")]
    ) && repair_lane.as_ref().is_some_and(|lane| {
        lane.required_target.as_deref() == Some("widget.py")
            && lane
                .contract_reconciliation
                .as_ref()
                .is_some_and(|decision| {
                    decision.owner == "SourceTestContractMismatch"
                        && decision.source_repair_allowed
                        && decision.test_repair_allowed
                })
            && lane
                .repair_control_snapshot
                .as_ref()
                .is_some_and(|snapshot| {
                    snapshot.repair_owner == "source_or_generated_test_by_contract_evidence"
                        && snapshot.required_target.as_deref() == Some("widget.py")
                })
    })
}

pub(crate) fn generated_test_parse_defect_active_work_matches_repair_lane_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test parse defect active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("widget.py"),
        Utf8PathBuf::from("test_widget.py"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test import has SyntaxError".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["test_widget".to_string()];
    state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-parse-defect-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
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
            observed: Some("SyntaxError: unterminated triple-quoted string literal".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `SyntaxError: unterminated triple-quoted string literal`"
                    .to_string(),
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

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed);
    let diagnostic = crate::agent::turn_decision::build_turn_decision_diagnostic(
        &state,
        active.as_ref(),
        &crate::agent::prompt::PromptPolicy::default(),
        &allowed,
        Some("auto".to_string()),
    );

    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("test_widget.py")]
    ) && diagnostic.active_targets == vec![Utf8PathBuf::from("test_widget.py")]
        && diagnostic
            .active_work_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("`test_widget.py`"))
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner == "generated_test"
                    && snapshot.required_target.as_deref() == Some("test_widget.py")
                    && snapshot
                        .forbidden_actions
                        .iter()
                        .any(|action| action == "stale_tool:shell")
            })
}

pub(crate) fn no_tests_ran_recent_generated_test_filechange_preserves_target_fixture_passes() -> bool
{
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "no tests ran generated-test target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create `component.py` and `test_component.py`, then run `python -m unittest`.".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("test_component.py")),
                    summary: "Added test_component.py".to_string(),
                }],
                summary: "Added test_component.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -X utf8 -m unittest".to_string(),
                output_text: "Ran 0 tests in 0.000s\n\nNO TESTS RAN".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("no-tests-ran".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "Command: python -X utf8 -m unittest\n\nStderr:\n----------------------------------------------------------------------\nRan 0 tests in 0.000s\n\nNO TESTS RAN".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-no-tests-ran-recent-generated-test".to_string(),
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
                            test_refs: Vec::new(),
                            sibling_obligations: Vec::new(),
                            evidence_markers: vec!["no_tests_ran".to_string()],
                        }],
                        sibling_obligations: Vec::new(),
                        source_refs: Vec::new(),
                        test_refs: Vec::new(),
                    }),
                    satisfies_command_identities: vec!["python -m unittest".to_string()],
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed);
    state.process_phase == ProcessPhase::Repair
        && state.active_targets == vec![Utf8PathBuf::from("test_component.py")]
        && state
            .failure
            .as_ref()
            .is_some_and(|failure| failure.targets == vec![Utf8PathBuf::from("test_component.py")])
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("test_component.py")]
        )
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("test_component.py")
                    && !snapshot.selected_recovery_action.starts_with("fail_closed")
            })
}

pub(crate) fn generated_test_local_binding_contradiction_active_work_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let workspace_root = std::env::temp_dir().join(format!("moyai-local-binding-{session_id}"));
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(workspace_root) else {
        return false;
    };
    if fs::create_dir_all(&workspace_root).is_err() {
        return false;
    }
    let source_path = workspace_root.join("widget.py");
    let test_path = workspace_root.join("test_widget.py");
    if fs::write(
        &source_path,
        "def public_tuple():\n    return \"alpha\", \"+\", \"omega\"\n",
    )
    .is_err()
    {
        return false;
    }
    if fs::write(
        &test_path,
        "import unittest\nimport widget\n\nclass TestGenerated(unittest.TestCase):\n    def test_public_tuple_contract(self):\n        first, marker, first = widget.public_tuple()\n        self.assertEqual(first, \"alpha\")\n",
    )
    .is_err()
    {
        return false;
    }

    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "generated-test local binding contradiction".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace_root.clone(),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-local-binding-before-enrichment".to_string(),
        failing_labels: vec!["test_public_tuple_contract".to_string()],
        primary_failure: Some("Command: python -X utf8 -m unittest".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_public_tuple_contract".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("alpha".to_string()),
            observed: Some("omega".to_string()),
            public_state_assertions: vec!["first".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["public_state_assertion_mismatch".to_string()],
            sibling_obligations: vec!["first".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["first".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    };
    let output_summary = "F\n\
======================================================================\n\
FAIL: test_public_tuple_contract (test_widget.TestGenerated.test_public_tuple_contract)\n\
----------------------------------------------------------------------\n\
Traceback (most recent call last):\n\
  File \"test_widget.py\", line 7, in test_public_tuple_contract\n\
    self.assertEqual(first, \"alpha\")\n\
AssertionError: 'omega' != 'alpha'\n\
\n\
----------------------------------------------------------------------\n\
Ran 1 test in 0.001s\n\
\n\
FAILED (failures=1)\n";
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("widget.py")),
                        summary: "Added widget.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_widget.py")),
                        summary: "Added test_widget.py".to_string(),
                    },
                ],
                summary: "Added widget.py; Added test_widget.py".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: python -X utf8 -m unittest".to_string(),
                output_text: output_summary.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: output_summary.to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: vec!["python -m unittest".to_string()],
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let allowed_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repair_lane = crate::agent::repair_lane::project_repair_lane(&state, &allowed_tools);
    let _ = fs::remove_dir_all(&workspace_root);

    matches!(state.process_phase, ProcessPhase::Repair)
        && state.active_targets == vec![Utf8PathBuf::from("test_widget.py")]
        && state
            .verification
            .failure_cluster
            .as_ref()
            .is_some_and(|cluster| {
                cluster.evidence.iter().any(|evidence| {
                    evidence
                        .evidence_markers
                        .iter()
                        .any(|marker| marker == "generated_test_local_binding_contradiction")
                })
            })
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner == "generated_test"
                    && snapshot.required_target.as_deref() == Some("test_widget.py")
            })
}

pub(crate) fn public_class_attribute_cluster_fixture() -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: "fixture-public-class-attribute".to_string(),
        failing_labels: vec!["test_calculate_add".to_string()],
        primary_failure: Some(".E".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("test_calculate_add".to_string()),
            target: Some("component.py".to_string()),
            symbol: Some("component.calculate".to_string()),
            call_site: Some("component.calculate(\"10 + 5\")".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("15".to_string()),
            observed: Some("component.calculate is missing".to_string()),
            public_state_assertions: vec!["component.calculate(\"10 + 5\")".to_string()],
            public_missing_attributes: vec!["component.calculate".to_string()],
            evidence_markers: vec![
                "public_class_attribute_mismatch".to_string(),
                "public missing method `component.calculate`".to_string(),
                "generated-test conflict evidence".to_string(),
            ],
            sibling_obligations: vec![
                "`component.calculate` is missing".to_string(),
                "component.calculate(\"10 + 5\")".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: vec!["component.py".to_string(), "10 + 5".to_string()],
            test_refs: vec!["test_component.py".to_string()],
        }],
        sibling_obligations: vec![
            "`component.calculate` is missing".to_string(),
            "component.calculate(\"10 + 5\")".to_string(),
        ],
        source_refs: vec!["component.py".to_string(), "10 + 5".to_string()],
        test_refs: vec!["test_component.py".to_string()],
    }
}
fn is_scenario_contract_ref(target: &str) -> bool {
    let name = target.rsplit(['/', '\\']).next().unwrap_or(target);
    matches!(
        name.to_ascii_lowercase().as_str(),
        "scenario_contract.md" | "scenario_contract.json"
    )
}

fn canonical_target_key(target: &str) -> String {
    target.replace('\\', "/").to_ascii_lowercase()
}

fn protected_artifact_targets_from_text_as_paths(text: &str) -> Vec<Utf8PathBuf> {
    extract_protected_artifact_targets(text)
        .into_iter()
        .map(Utf8PathBuf::from)
        .collect()
}

fn filter_protected_reference_targets(
    targets: Vec<Utf8PathBuf>,
    protected_targets: &[Utf8PathBuf],
) -> Vec<Utf8PathBuf> {
    if protected_targets.is_empty() {
        return targets;
    }
    let protected = protected_targets
        .iter()
        .map(|target| canonical_target_key(target.as_str()))
        .collect::<BTreeSet<_>>();
    targets
        .into_iter()
        .filter(|target| !protected.contains(&canonical_target_key(target.as_str())))
        .collect()
}

fn observed_target_set_contains(observed_targets: &BTreeSet<String>, target: &Utf8PathBuf) -> bool {
    observed_targets.contains(&target.as_str().to_ascii_lowercase())
}

fn observed_target_set_contains_path(
    observed_targets: &BTreeSet<String>,
    target: &Utf8PathBuf,
    workspace_root: &Utf8Path,
) -> bool {
    normalize_target_path(target.as_str(), workspace_root)
        .map(|normalized| observed_targets.contains(&normalized.as_str().to_ascii_lowercase()))
        .unwrap_or_else(|| observed_target_set_contains(observed_targets, target))
}

fn retain_targets_without_observed_progress(
    targets: &mut Vec<Utf8PathBuf>,
    observed_targets: &BTreeSet<String>,
    workspace_root: &Utf8Path,
) {
    if observed_targets.is_empty() {
        return;
    }
    targets.retain(|target| {
        !observed_target_set_contains_path(observed_targets, target, workspace_root)
    });
}

pub(crate) fn docs_route_pending_repair_target(
    state: Option<&DocsRouteState>,
) -> Option<Utf8PathBuf> {
    docs_route_pending_repair_targets(state).into_iter().next()
}

pub(crate) fn docs_route_pending_repair_targets(
    state: Option<&DocsRouteState>,
) -> Vec<Utf8PathBuf> {
    let Some(state) = state else {
        return Vec::new();
    };
    if !state.pending_deliverables.is_empty() {
        return state
            .pending_deliverables
            .iter()
            .map(|item| item.target.clone())
            .collect();
    }
    docs_route_pending_deliverables_from_parts(
        &state.area_coverage,
        &state.deliverables,
        &state.factual_checks,
        state.active_deliverable.as_ref(),
    )
    .into_iter()
    .map(|item| item.target)
    .collect()
}

fn docs_route_pending_deliverables_from_state(
    state: Option<&DocsRouteState>,
) -> Vec<DocsPendingDeliverable> {
    let Some(state) = state else {
        return Vec::new();
    };
    if !state.pending_deliverables.is_empty() {
        return state.pending_deliverables.clone();
    }
    let targets = docs_route_pending_repair_targets(Some(state));
    targets
        .into_iter()
        .map(|target| DocsPendingDeliverable {
            target,
            summary: "docs route repair target".to_string(),
        })
        .collect()
}

fn docs_route_pending_deliverables_from_parts(
    area_coverage: &[DocsAreaCoverage],
    deliverables: &[DocsDeliverableCoverage],
    factual_checks: &[DocsFactCheck],
    active_deliverable: Option<&Utf8PathBuf>,
) -> Vec<DocsPendingDeliverable> {
    if area_coverage
        .iter()
        .any(|coverage| coverage.status == ContractStatus::Pending)
    {
        let missing = area_coverage
            .iter()
            .filter(|coverage| coverage.status == ContractStatus::Pending)
            .map(|coverage| docs_area_label(coverage.area))
            .collect::<Vec<_>>();
        if let Some(target) = active_deliverable.cloned().or_else(|| {
            deliverables
                .first()
                .map(|deliverable| deliverable.target.clone())
        }) {
            return vec![DocsPendingDeliverable {
                target,
                summary: format!("survey areas={}", missing.join(", ")),
            }];
        }
        return Vec::new();
    }

    let mut pending = deliverables
        .iter()
        .filter_map(|deliverable| {
            docs_route_missing_coverage_summary(deliverable).map(|summary| DocsPendingDeliverable {
                target: deliverable.target.clone(),
                summary,
            })
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        let pending_fact_summary = docs_route_pending_fact_summary(factual_checks);
        if let Some(summary) = pending_fact_summary {
            if let Some(target) = active_deliverable.cloned().or_else(|| {
                deliverables
                    .first()
                    .map(|deliverable| deliverable.target.clone())
            }) {
                pending.push(DocsPendingDeliverable { target, summary });
            }
        }
    }

    pending
}
fn docs_route_pending_fact_summary(factual_checks: &[DocsFactCheck]) -> Option<String> {
    let missing = factual_checks
        .iter()
        .filter(|check| check.status == ContractStatus::Pending)
        .map(|check| check.subject.replace('\\', "/"))
        .collect::<Vec<_>>();
    (!missing.is_empty()).then(|| format!("facts={}", missing.join(", ")))
}

fn docs_route_missing_coverage_summary(coverage: &DocsDeliverableCoverage) -> Option<String> {
    let missing_areas = docs_deliverable_missing_required_areas(coverage);
    let missing_topics = docs_deliverable_missing_required_topics(coverage).collect::<Vec<_>>();
    let missing_grounding = coverage
        .grounding
        .iter()
        .filter(|grounding| grounding.status == ContractStatus::Pending)
        .collect::<Vec<_>>();
    if missing_areas.is_empty() && missing_topics.is_empty() && missing_grounding.is_empty() {
        return None;
    }
    let mut items = Vec::new();
    if !missing_areas.is_empty() {
        items.push(format!(
            "areas={}",
            missing_areas
                .iter()
                .map(|area| docs_area_label(*area))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !missing_topics.is_empty() {
        items.push(format!("topics={}", missing_topics.join(", ")));
    }
    if !missing_grounding.is_empty() {
        items.push(format!(
            "anchors={}",
            missing_grounding
                .iter()
                .map(|grounding| {
                    let label = docs_grounding_requirement_label(grounding.requirement);
                    match grounding.representative_path.as_ref() {
                        Some(path) => format!("{label}:{}", path.as_str()),
                        None => label.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(format!("{} ({})", coverage.target, items.join("; ")))
}

fn docs_deliverable_missing_required_topics(
    coverage: &DocsDeliverableCoverage,
) -> impl Iterator<Item = String> + '_ {
    let satisfied = coverage
        .satisfied_topics
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    coverage
        .required_topics
        .iter()
        .filter(move |topic| !satisfied.contains(&topic.to_ascii_lowercase()))
        .cloned()
}
fn docs_area_catalog() -> [DocsArea; 5] {
    [
        DocsArea::Backend,
        DocsArea::Frontend,
        DocsArea::Tests,
        DocsArea::Data,
        DocsArea::Examples,
    ]
}
fn docs_area_label(area: DocsArea) -> &'static str {
    match area {
        DocsArea::Backend => "backend",
        DocsArea::Frontend => "frontend",
        DocsArea::Tests => "tests",
        DocsArea::Data => "data",
        DocsArea::Examples => "examples",
    }
}

fn docs_area_markers(area: DocsArea) -> &'static [&'static str] {
    match area {
        DocsArea::Backend => &["backend", "back-end", "バックエンド"],
        DocsArea::Frontend => &["frontend", "front-end", "フロントエンド"],
        DocsArea::Tests => &["tests", "test", "pytest", "unittest", "テスト"],
        DocsArea::Data => &["data", "dataset", "データ"],
        DocsArea::Examples => &["examples", "example", "sample", "サンプル"],
    }
}

fn docs_route_survey_packet_summary(required_areas: &[DocsArea]) -> String {
    if required_areas.is_empty() {
        return "docs-only route: required repository areas are derived from the request and observed workspace evidence; generated and dependency paths are not coverage authority".to_string();
    }
    let labels = required_areas
        .iter()
        .map(|area| docs_area_label(*area))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "docs-only route: required areas={labels}; use concrete evidence from those areas; generated and dependency paths are not coverage authority"
    )
}
fn path_matches_docs_area(path: &Utf8Path, area: DocsArea) -> bool {
    let normalized = path.as_str().replace('\\', "/");
    if docs_route_path_is_generated_or_dependency(path) {
        return false;
    }
    match area {
        DocsArea::Backend => {
            (normalized == "backend" || normalized.starts_with("backend/"))
                && !normalized.starts_with("backend/tests/")
                && !normalized.starts_with("backend/data/")
        }
        DocsArea::Frontend => {
            (normalized == "frontend" || normalized.starts_with("frontend/"))
                && !normalized.starts_with("frontend/tests/")
                && !normalized.starts_with("frontend/data/")
        }
        DocsArea::Tests => {
            normalized == "tests"
                || normalized == "backend/tests"
                || normalized == "frontend/tests"
                || normalized.starts_with("tests/")
                || normalized.starts_with("backend/tests/")
                || normalized.starts_with("frontend/tests/")
                || docs_route_path_is_flat_test_artifact(path)
        }
        DocsArea::Data => {
            normalized == "data"
                || normalized == "backend/data"
                || normalized == "frontend/data"
                || normalized.starts_with("data/")
                || normalized.starts_with("backend/data/")
                || normalized.starts_with("frontend/data/")
        }
        DocsArea::Examples => normalized == "examples" || normalized.starts_with("examples/"),
    }
}

pub(crate) fn docs_route_path_is_generated_or_dependency(path: &Utf8Path) -> bool {
    let normalized = path.as_str().replace('\\', "/").to_ascii_lowercase();
    normalized.split('/').any(|part| {
        matches!(
            part,
            ".git"
                | ".moyai"
                | ".venv"
                | "venv"
                | "node_modules"
                | "__pycache__"
                | ".pytest_cache"
                | "target"
                | "dist"
                | "build"
                | "coverage"
                | "playwright-report"
                | "test-results"
        ) || part.ends_with(".egg-info")
    }) || docs_route_path_has_generated_data_prefix(&normalized)
        || normalized.contains("/generated/")
        || normalized.contains("/cache/")
        || normalized.ends_with(".db")
        || normalized.ends_with(".sqlite")
        || normalized.ends_with(".sqlite3")
        || normalized.ends_with(".pdf")
}

fn docs_route_path_is_flat_test_artifact(path: &Utf8Path) -> bool {
    if path.components().count() != 1 {
        return false;
    }
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let lower = file_name.to_ascii_lowercase();
    let Some(stem) = path.file_stem() else {
        return false;
    };
    let stem = stem.to_ascii_lowercase();
    let extension = path
        .extension()
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let conventional_extension = matches!(
        extension.as_str(),
        "py" | "rs" | "go" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt" | "cs" | "rb"
    );
    conventional_extension
        && (stem.starts_with("test_")
            || stem.ends_with("_test")
            || lower.contains(".test.")
            || lower.contains(".spec.")
            || lower == "tests.rs")
}

fn docs_route_path_has_generated_data_prefix(normalized: &str) -> bool {
    [
        "data/e2e/",
        "data/memory/",
        "data/runs/",
        "data/documents/",
        "data/smoke/",
        "data/reports/",
        "backend/data/e2e/",
        "backend/data/memory/",
        "backend/data/runs/",
        "backend/data/documents/",
        "backend/data/smoke/",
        "backend/data/reports/",
        "frontend/data/e2e/",
        "frontend/data/memory/",
        "frontend/data/runs/",
        "frontend/data/documents/",
        "frontend/data/smoke/",
        "frontend/data/reports/",
    ]
    .iter()
    .any(|prefix| normalized == prefix.trim_end_matches('/') || normalized.starts_with(prefix))
        || [
            "/data/e2e/",
            "/data/memory/",
            "/data/runs/",
            "/data/documents/",
            "/data/smoke/",
            "/data/reports/",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn collect_docs_area_representative_paths(
    workspace_root: &Utf8Path,
    area: DocsArea,
) -> Vec<Utf8PathBuf> {
    let mut found = Vec::new();
    collect_docs_representative_paths_inner(workspace_root, workspace_root, area, 0, &mut found);
    found.sort();
    found.truncate(4);
    found
}

fn collect_docs_representative_paths_inner(
    workspace_root: &Utf8Path,
    current: &Utf8Path,
    area: DocsArea,
    depth: usize,
    found: &mut Vec<Utf8PathBuf>,
) {
    if depth > 4 || found.len() >= 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(current.as_std_path()) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let relative = path
            .strip_prefix(workspace_root)
            .map(|value| value.to_path_buf())
            .unwrap_or_else(|_| path.clone());
        if docs_route_path_is_generated_or_dependency(relative.as_path()) {
            continue;
        }
        if path.is_dir() {
            if path_matches_docs_area(relative.as_path(), area) {
                found.push(relative.clone());
            }
            collect_docs_representative_paths_inner(workspace_root, &path, area, depth + 1, found);
        } else if path.is_file() && path_matches_docs_area(relative.as_path(), area) {
            found.push(relative);
        }
        if found.len() >= 4 {
            break;
        }
    }
}

fn docs_representative_paths_mentioned_in_text(
    workspace_root: &Utf8Path,
    text: &str,
) -> Vec<Utf8PathBuf> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for area in docs_area_catalog() {
        if !lower.contains(docs_area_label(area)) {
            continue;
        }
        found.extend(
            collect_docs_area_representative_paths(workspace_root, area)
                .into_iter()
                .take(2),
        );
    }
    let mut seen = BTreeSet::new();
    found
        .into_iter()
        .filter(|path| seen.insert(path.as_str().to_string()))
        .collect()
}

fn docs_grounding_requirements_for_areas(
    workspace_root: &Utf8Path,
    required_areas: &[DocsArea],
) -> Vec<(DocsGroundingRequirement, Vec<&'static str>)> {
    let mut requirements = Vec::new();
    for area in required_areas {
        let candidates = match area {
            DocsArea::Backend => vec![
                DocsGroundingRequirement::BackendMetadata,
                DocsGroundingRequirement::BackendSource,
                DocsGroundingRequirement::BackendRoute,
            ],
            DocsArea::Frontend => vec![
                DocsGroundingRequirement::FrontendMetadata,
                DocsGroundingRequirement::FrontendSource,
            ],
            DocsArea::Tests => vec![DocsGroundingRequirement::Tests],
            DocsArea::Data => vec![DocsGroundingRequirement::Data],
            DocsArea::Examples => vec![DocsGroundingRequirement::Examples],
        };
        for requirement in candidates {
            let paths = docs_grounding_candidate_paths(requirement);
            if paths.iter().any(|path| workspace_root.join(*path).exists()) {
                requirements.push((requirement, paths.to_vec()));
            }
        }
    }
    requirements
}

fn docs_grounding_candidate_paths(
    requirement: DocsGroundingRequirement,
) -> &'static [&'static str] {
    match requirement {
        DocsGroundingRequirement::BackendMetadata => &[
            "backend/pyproject.toml",
            "backend/package.json",
            "backend/Cargo.toml",
        ],
        DocsGroundingRequirement::BackendSource => {
            &["backend/app", "backend/src", "backend/main.py"]
        }
        DocsGroundingRequirement::BackendRoute => {
            &["backend/app/api", "backend/app/routes", "backend/routes"]
        }
        DocsGroundingRequirement::FrontendMetadata => &[
            "frontend/package.json",
            "frontend/vite.config.ts",
            "frontend/next.config.js",
        ],
        DocsGroundingRequirement::FrontendSource => {
            &["frontend/app", "frontend/src", "frontend/pages"]
        }
        DocsGroundingRequirement::Examples => &["examples"],
        DocsGroundingRequirement::Tests => &["tests", "backend/tests", "frontend/tests"],
        DocsGroundingRequirement::Data => &["data", "backend/data", "frontend/data"],
    }
}
fn docs_grounding_requirement_label(requirement: DocsGroundingRequirement) -> &'static str {
    match requirement {
        DocsGroundingRequirement::BackendMetadata => "backend project metadata",
        DocsGroundingRequirement::BackendSource => "backend source entry/config",
        DocsGroundingRequirement::BackendRoute => "backend route source",
        DocsGroundingRequirement::FrontendMetadata => "frontend package metadata",
        DocsGroundingRequirement::FrontendSource => "frontend route/component source",
        DocsGroundingRequirement::Examples => "examples sample",
        DocsGroundingRequirement::Tests => "test file",
        DocsGroundingRequirement::Data => "data artifact",
    }
}
fn docs_deliverable_missing_required_areas(coverage: &DocsDeliverableCoverage) -> Vec<DocsArea> {
    let present = coverage
        .representative_paths
        .iter()
        .cloned()
        .chain(
            coverage
                .grounding
                .iter()
                .filter(|grounding| grounding.status == ContractStatus::Satisfied)
                .filter_map(|grounding| grounding.representative_path.clone()),
        )
        .flat_map(|path| {
            docs_area_catalog()
                .into_iter()
                .filter(move |area| path_matches_docs_area(path.as_path(), *area))
        })
        .collect::<BTreeSet<_>>();
    coverage
        .required_areas
        .iter()
        .copied()
        .filter(|area| !present.contains(area))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuredDocumentSummaryContract {
    expected_files: Vec<String>,
    output_target: String,
    batch_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct StructuredDocumentSummaryProgress {
    processed_files: Vec<String>,
    batch_sizes: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructuredDocumentSummarySnapshot {
    pub output_target: String,
    pub expected_files: Vec<String>,
    pub processed_files: Vec<String>,
    pub missing_files: Vec<String>,
    pub batch_size: Option<usize>,
    pub expected_batch_sizes: Vec<usize>,
    pub observed_batch_sizes: Vec<usize>,
    pub current_batch_expected: Option<usize>,
    pub current_batch_processed: usize,
}

pub(crate) fn structured_document_summary_snapshot(
    transcript: &Transcript,
    latest_user_text: Option<&str>,
) -> Option<StructuredDocumentSummarySnapshot> {
    let latest_user_text = latest_user_text?;
    let contract =
        structured_document_summary_contract(latest_user_text, transcript.session.cwd.as_path())?;
    let progress = structured_document_summary_progress(transcript, &contract);
    let processed_set = progress
        .processed_files
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let missing_files = contract
        .expected_files
        .iter()
        .filter(|value| !processed_set.contains(&value.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let expected_batch_sizes = contract
        .batch_size
        .map(|batch_size| expected_batch_sizes(contract.expected_files.len(), batch_size))
        .unwrap_or_default();
    let completed_batch_total = progress.batch_sizes.iter().sum::<usize>();
    let current_batch_processed = progress
        .processed_files
        .len()
        .saturating_sub(completed_batch_total);
    let current_batch_expected = expected_batch_sizes
        .get(progress.batch_sizes.len())
        .copied();

    Some(StructuredDocumentSummarySnapshot {
        output_target: contract.output_target,
        expected_files: contract.expected_files,
        processed_files: progress.processed_files,
        missing_files,
        batch_size: contract.batch_size,
        expected_batch_sizes,
        observed_batch_sizes: progress.batch_sizes,
        current_batch_expected,
        current_batch_processed,
    })
}

fn structured_document_summary_snapshot_from_history_items(
    workspace_root: &Utf8Path,
    history_items: &[HistoryItem],
    latest_user_text: Option<&str>,
) -> Option<StructuredDocumentSummarySnapshot> {
    let latest_user_text = latest_user_text?;
    let contract = structured_document_summary_contract(latest_user_text, workspace_root)?;
    let mut progress =
        structured_document_summary_progress_from_history_items(history_items, &contract);
    let mut processed = progress
        .processed_files
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for file in structured_document_processed_files_from_output(workspace_root, &contract) {
        if processed.insert(file.clone()) {
            progress.processed_files.push(file);
        }
    }
    let processed_set = progress
        .processed_files
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let missing_files = contract
        .expected_files
        .iter()
        .filter(|value| !processed_set.contains(&value.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let expected_batch_sizes = contract
        .batch_size
        .map(|batch_size| expected_batch_sizes(contract.expected_files.len(), batch_size))
        .unwrap_or_default();
    let completed_batch_total = progress.batch_sizes.iter().sum::<usize>();
    let current_batch_processed = progress
        .processed_files
        .len()
        .saturating_sub(completed_batch_total);
    let current_batch_expected = expected_batch_sizes
        .get(progress.batch_sizes.len())
        .copied();

    Some(StructuredDocumentSummarySnapshot {
        output_target: contract.output_target,
        expected_files: contract.expected_files,
        processed_files: progress.processed_files,
        missing_files,
        batch_size: contract.batch_size,
        expected_batch_sizes,
        observed_batch_sizes: progress.batch_sizes,
        current_batch_expected,
        current_batch_processed,
    })
}

fn structured_document_processed_files_from_output(
    workspace_root: &Utf8Path,
    contract: &StructuredDocumentSummaryContract,
) -> Vec<String> {
    let path = workspace_root.join(contract.output_target.as_str());
    let Ok(content) = fs::read_to_string(path.as_std_path()) else {
        return Vec::new();
    };
    let lower = content.to_ascii_lowercase();
    contract
        .expected_files
        .iter()
        .filter(|file| {
            let file_lower = file.to_ascii_lowercase();
            lower.contains(&format!("### {file_lower}"))
                || lower.contains(&format!("#### {file_lower}"))
                || lower.contains(&file_lower)
        })
        .cloned()
        .collect()
}

fn structured_document_summary_contract(
    text: &str,
    workspace_root: &Utf8Path,
) -> Option<StructuredDocumentSummaryContract> {
    if !looks_like_structured_document_work(Some(text)) {
        return None;
    }

    let requested = requested_work_contract_from_instruction_text(text);
    let output_target = requested.deliverable_targets.into_iter().find(|target| {
        target
            .rsplit_once('.')
            .map(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "md" | "txt"))
            .unwrap_or(false)
    })?;
    let mut extensions = mentioned_structured_document_extensions(text);
    if let Some(output_extension) = Utf8Path::new(output_target.as_str()).extension() {
        extensions.remove(&output_extension.to_ascii_lowercase());
    }
    if extensions.is_empty() {
        return None;
    }
    let mut expected_files = collect_structured_document_targets(workspace_root, &extensions);
    expected_files.retain(|target| !target.eq_ignore_ascii_case(&output_target));
    if expected_files.len() < 2 {
        return None;
    }

    Some(StructuredDocumentSummaryContract {
        expected_files,
        output_target,
        batch_size: explicit_batch_size(text),
    })
}

fn mentioned_structured_document_extensions(text: &str) -> BTreeSet<String> {
    let lower = text.to_ascii_lowercase();
    let mut extensions = BTreeSet::new();
    for extension in ["pdf", "docx", "xlsx", "pptx", "html", "csv"] {
        if lower.contains(extension) {
            extensions.insert(extension.to_string());
        }
    }
    extensions
}

fn collect_structured_document_targets(
    root: &Utf8Path,
    extensions: &BTreeSet<String>,
) -> Vec<String> {
    let mut targets = BTreeSet::new();
    collect_structured_document_targets_recursive(root, root, extensions, &mut targets);
    targets.into_iter().collect()
}

fn collect_structured_document_targets_recursive(
    root: &Utf8Path,
    current: &Utf8Path,
    extensions: &BTreeSet<String>,
    targets: &mut BTreeSet<String>,
) {
    let Ok(entries) = fs::read_dir(current.as_std_path()) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_structured_document_targets_recursive(
                root,
                path.as_path(),
                extensions,
                targets,
            );
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(extension) = path.extension() else {
            continue;
        };
        if !extensions.contains(&extension.to_ascii_lowercase()) {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .as_str()
            .replace('\\', "/");
        targets.insert(relative);
    }
}

fn explicit_batch_size(text: &str) -> Option<usize> {
    for pattern in [
        r"(?i)\b(\d+)\s*files?\s+at\s+a\s+time\b",
        r"(?i)\bbatch(?:es)?\s+of\s+(\d+)\b",
        r"(\d+)\s*ファイルずつ",
    ] {
        let regex = Regex::new(pattern).expect("batch-size regex should compile");
        if let Some(value) = regex
            .captures(text)
            .and_then(|captures| captures.get(1))
            .and_then(|value| value.as_str().parse::<usize>().ok())
        {
            return Some(value);
        }
    }
    None
}

fn structured_document_summary_progress(
    transcript: &Transcript,
    contract: &StructuredDocumentSummaryContract,
) -> StructuredDocumentSummaryProgress {
    let Some(latest_user_index) =
        transcript
            .messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| {
                matches!(message.record.role, MessageRole::User).then_some(index)
            })
    else {
        return StructuredDocumentSummaryProgress::default();
    };

    let expected = contract
        .expected_files
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let tool_calls = tool_calls_by_call_id(transcript);
    let mut processed = BTreeSet::new();
    let mut pending_batch = BTreeSet::new();
    let mut batch_sizes = Vec::new();
    let output_target = contract.output_target.to_ascii_lowercase();

    for message in &transcript.messages[latest_user_index + 1..] {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolResult(value)
                    if is_successful_docling_conversion_result(value) =>
                {
                    let Some(call) = tool_calls.get(&value.tool_call_id) else {
                        continue;
                    };
                    if call.tool_name != ToolName::DoclingConvert {
                        continue;
                    }
                    let Some(target) = extract_docling_target(&call.arguments_json) else {
                        continue;
                    };
                    let normalized = target.to_ascii_lowercase();
                    if !expected.contains(&normalized) {
                        continue;
                    }
                    if processed.insert(normalized.clone()) {
                        pending_batch.insert(normalized);
                    }
                }
                MessagePart::DiffSummary(value)
                    if value.summary.to_ascii_lowercase().contains(&output_target) =>
                {
                    if !pending_batch.is_empty() {
                        batch_sizes.push(pending_batch.len());
                        pending_batch.clear();
                    }
                }
                _ => {}
            }
        }
    }

    let processed_files = contract
        .expected_files
        .iter()
        .filter(|value| processed.contains(&value.to_ascii_lowercase()))
        .cloned()
        .collect();
    StructuredDocumentSummaryProgress {
        processed_files,
        batch_sizes,
    }
}

fn structured_document_summary_progress_from_history_items(
    history_items: &[HistoryItem],
    contract: &StructuredDocumentSummaryContract,
) -> StructuredDocumentSummaryProgress {
    let Some(latest_user_sequence) = latest_user_turn_sequence(history_items) else {
        return StructuredDocumentSummaryProgress::default();
    };

    let expected = contract
        .expected_files
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut tool_calls: HashMap<ToolCallId, (ToolName, Value)> = HashMap::new();
    let mut processed = BTreeSet::new();
    let mut pending_batch = BTreeSet::new();
    let mut batch_sizes = Vec::new();
    let output_target = contract.output_target.to_ascii_lowercase();

    for item in history_items_in_sequence(history_items)
        .into_iter()
        .filter(|item| history_item_order_scalar(item) > latest_user_sequence)
    {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let args = crate::protocol::canonical_tool_call_arguments(
                    arguments,
                    model_arguments,
                    effective_arguments,
                )
                .clone();
                tool_calls.insert(*call_id, (*tool, args));
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                ..
            } if *status == ToolLifecycleStatus::Completed && success.unwrap_or(true) => {
                let Some((tool, args)) = tool_calls.get(call_id) else {
                    continue;
                };
                if *tool != ToolName::DoclingConvert {
                    continue;
                }
                let Some(target) = extract_docling_target(&args.to_string()) else {
                    continue;
                };
                let normalized = target.to_ascii_lowercase();
                if !expected.contains(&normalized) {
                    continue;
                }
                if processed.insert(normalized.clone()) {
                    pending_batch.insert(normalized);
                }
            }
            HistoryItemPayload::FileChange {
                changes, summary, ..
            } => {
                let output_changed = changes.iter().any(|change| {
                    change
                        .path_after
                        .as_ref()
                        .or(change.path_before.as_ref())
                        .map(|path| path.as_str().replace('\\', "/").to_ascii_lowercase())
                        .is_some_and(|path| path.ends_with(&output_target))
                }) || summary.to_ascii_lowercase().contains(&output_target);
                if output_changed && !pending_batch.is_empty() {
                    batch_sizes.push(pending_batch.len());
                    pending_batch.clear();
                }
            }
            _ => {}
        }
    }

    let processed_files = contract
        .expected_files
        .iter()
        .filter(|value| processed.contains(&value.to_ascii_lowercase()))
        .cloned()
        .collect();
    StructuredDocumentSummaryProgress {
        processed_files,
        batch_sizes,
    }
}

pub(crate) fn message_user_structured_document_progress_fixture_passes() -> bool {
    let unique = format!(
        "moyai-state-structured-doc-message-user-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let root_path = std::env::temp_dir().join(unique);
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let result = (|| -> bool {
        if fs::create_dir_all(workspace_root.as_std_path()).is_err()
            || fs::write(workspace_root.join("a.pdf").as_std_path(), b"a").is_err()
            || fs::write(workspace_root.join("b.pdf").as_std_path(), b"b").is_err()
        {
            return false;
        }
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let call_id = ToolCallId::new();
        let user_text =
            "Summarize all pdf files into summary.md in batches of 1 file at a time.".to_string();
        let history_items = vec![
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 1,
                payload: HistoryItemPayload::Message {
                    message_id: None,
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: user_text.clone(),
                    }],
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
                    tool: ToolName::DoclingConvert,
                    arguments: json!({"path": "a.pdf"}),
                    model_arguments: Value::Null,
                    effective_arguments: Value::Null,
                    adjusted_arguments: None,
                    permission_decision: None,
                    sandbox_decision: None,
                    allowed_surface: Vec::new(),
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
                    title: "Docling converted a.pdf".to_string(),
                    output_text: "converted".to_string(),
                    metadata: Value::Null,
                    success: Some(true),
                    progress_effect: ToolProgressEffect::MadeProgress,
                    blocked_action: None,
                    result_hash: Some("structured-doc-message-user".to_string()),
                    verification_run: None,
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 4,
                created_at_ms: 4,
                payload: HistoryItemPayload::FileChange {
                    change_ids: Vec::new(),
                    changes: vec![FileChangeEvidence {
                        change_id: ChangeId::new(),
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("summary.md")),
                        kind: crate::session::ChangeKind::Add,
                        summary: "summary.md updated".to_string(),
                    }],
                    summary: "summary.md updated".to_string(),
                },
            },
        ];
        let Some(snapshot) = structured_document_summary_snapshot_from_history_items(
            workspace_root.as_path(),
            &history_items,
            Some(&user_text),
        ) else {
            return false;
        };
        snapshot.processed_files == vec!["a.pdf".to_string()]
            && snapshot.missing_files == vec!["b.pdf".to_string()]
            && snapshot.observed_batch_sizes == vec![1]
    })();
    let _ = fs::remove_dir_all(workspace_root.as_std_path());
    result
}

fn expected_batch_sizes(total: usize, batch_size: usize) -> Vec<usize> {
    if total == 0 || batch_size == 0 {
        return Vec::new();
    }
    let mut remaining = total;
    let mut batches = Vec::new();
    while remaining > 0 {
        let current = remaining.min(batch_size);
        batches.push(current);
        remaining -= current;
    }
    batches
}

fn is_successful_docling_conversion_result(value: &crate::session::ToolResultPart) -> bool {
    value.status == crate::session::ToolCallStatus::Completed
        && (value.title.starts_with("Docling converted ") || value.title.starts_with("Converted "))
}

fn extract_docling_target(arguments_json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(arguments_json).ok()?;
    let path = value.get("path")?.as_str()?;
    let normalized = path.replace('\\', "/");
    let parsed = Utf8Path::new(&normalized);
    let relative = parsed
        .file_name()
        .map(str::to_string)
        .unwrap_or_else(|| parsed.as_str().to_string());
    Some(relative)
}

fn extract_failure_paths_from_text(summary: &str, workspace_root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut targets = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some(path) = extract_python_traceback_path(trimmed)
            .and_then(|value| normalize_target_path(&value, workspace_root))
        {
            targets.push(path);
        }
    }
    targets.extend(extract_import_error_module_paths_from_text(
        summary,
        workspace_root,
    ));
    prioritize_repair_targets(targets)
}

fn extract_import_error_module_paths_from_text(
    summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let mut targets = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some(path) = extract_import_error_module_path(trimmed)
            .and_then(|value| normalize_target_path(&value, workspace_root))
        {
            targets.push(path);
        }
    }
    prioritize_repair_targets(targets)
}

fn extract_import_error_module_path(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("importerror:")
        || !lower.contains("cannot import name")
        || !lower.contains(" from ")
    {
        return None;
    }

    let start = line.rfind('(')?;
    let end = line[start + 1..].find(')')? + start + 1;
    let candidate = line[start + 1..end].trim();
    if candidate.is_empty() {
        return None;
    }
    let normalized = candidate.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".py").then(|| candidate.to_string())
}

fn extract_python_traceback_path(line: &str) -> Option<String> {
    if let Some(rest) = line.split_once("File \"").map(|(_, rest)| rest) {
        return rest.split('"').next().map(str::to_string);
    }
    if let Some(rest) = line.split_once("File '").map(|(_, rest)| rest) {
        return rest.split('\'').next().map(str::to_string);
    }
    None
}

fn normalize_target_path(target: &str, workspace_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let normalized = crate::workspace::project::normalize_path_separators(target);
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        return None;
    }
    let workspace_root = workspace_root_for_target_matching(workspace_root);
    let path = Utf8Path::new(trimmed);
    let absolute = if path.is_absolute() {
        lexical_normalize_utf8_path(path)
    } else if workspace_root.as_str().is_empty() {
        lexical_normalize_utf8_path(path)
    } else {
        lexical_normalize_utf8_path(workspace_root.join(path).as_path())
    };
    if workspace_root.as_str().is_empty() {
        return Some(path_with_forward_slashes(absolute.as_path()));
    }
    absolute
        .strip_prefix(workspace_root.as_path())
        .map(path_with_forward_slashes)
        .ok()
        .or_else(|| {
            strip_workspace_prefix_case_insensitive(absolute.as_path(), workspace_root.as_path())
        })
        .or_else(|| (!path.is_absolute()).then(|| path_with_forward_slashes(path)))
}

fn workspace_root_for_target_matching(workspace_root: &Utf8Path) -> Utf8PathBuf {
    let normalized_root =
        crate::workspace::project::normalize_path_separators(workspace_root.as_str());
    let trimmed = normalized_root.trim();
    if trimmed.is_empty() {
        return Utf8PathBuf::new();
    }
    let root = Utf8Path::new(trimmed);
    if root.is_absolute() {
        return lexical_normalize_utf8_path(root);
    }
    std::env::current_dir()
        .ok()
        .and_then(|cwd| Utf8PathBuf::from_path_buf(cwd).ok())
        .map(|cwd| lexical_normalize_utf8_path(cwd.join(root).as_path()))
        .unwrap_or_else(|| lexical_normalize_utf8_path(root))
}

fn lexical_normalize_utf8_path(path: &Utf8Path) -> Utf8PathBuf {
    let mut normalized = PathBuf::new();
    for component in Path::new(path.as_str()).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Utf8PathBuf::from_path_buf(normalized).unwrap_or_else(|_| path.to_path_buf())
}

fn path_with_forward_slashes(path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(path.as_str().replace('\\', "/"))
}

fn strip_workspace_prefix_case_insensitive(
    absolute: &Utf8Path,
    workspace_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    let absolute_key = absolute.as_str().replace('\\', "/");
    let root_key = workspace_root.as_str().replace('\\', "/");
    let root_key = root_key.trim_end_matches('/');
    if root_key.is_empty() {
        return Some(Utf8PathBuf::from(absolute_key));
    }
    let absolute_lower = absolute_key.to_ascii_lowercase();
    let root_lower = root_key.to_ascii_lowercase();
    if absolute_lower == root_lower {
        return Some(Utf8PathBuf::new());
    }
    let prefix = format!("{root_lower}/");
    if !absolute_lower.starts_with(&prefix) {
        return None;
    }
    let relative = absolute_key[root_key.len()..].trim_start_matches('/');
    Some(Utf8PathBuf::from(relative))
}

fn prioritize_repair_targets(targets: Vec<Utf8PathBuf>) -> Vec<Utf8PathBuf> {
    let mut seen = BTreeSet::new();
    let mut implementation = Vec::new();
    let mut documentation = Vec::new();
    let mut unknown = Vec::new();

    for target in targets {
        let key = target.as_str().replace('\\', "/");
        if !seen.insert(key) {
            continue;
        }
        if is_documentation_target(&target) {
            documentation.push(target);
        } else if is_code_or_test_target(&target) {
            implementation.push(target);
        } else {
            unknown.push(target);
        }
    }

    if !implementation.is_empty() {
        implementation.extend(unknown);
        return implementation;
    }

    if !unknown.is_empty() {
        unknown.extend(documentation);
        return unknown;
    }

    documentation
}

fn is_documentation_target(path: &Utf8Path) -> bool {
    let lower = path.as_str().replace('\\', "/").to_ascii_lowercase();
    lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.ends_with(".md")
        || lower.ends_with(".rst")
        || lower.ends_with(".adoc")
}

fn is_code_or_test_target(path: &Utf8Path) -> bool {
    let lower = path.as_str().replace('\\', "/").to_ascii_lowercase();
    lower.contains("/src/")
        || lower.contains("/tests/")
        || lower.ends_with(".py")
        || lower.ends_with(".rs")
        || lower.ends_with(".js")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".jsx")
        || lower
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .starts_with("test_")
}

fn is_test_focus_target(path: &Utf8Path) -> bool {
    let lower = path.as_str().replace('\\', "/").to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.ends_with("_test.py")
        || lower.ends_with("test_integration.py")
        || lower.ends_with("integration_test.py")
        || lower
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .starts_with("test_")
}

pub(crate) fn docs_route_verification_failure_preserves_docs_active_target_fixture_passes() -> bool
{
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "docs route verification target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.route = TaskRoute::Docs;
    previous.process_phase = ProcessPhase::Repair;
    previous.active_targets = vec![Utf8PathBuf::from("docs/calculator-design.md")];
    previous.completion.route_contract_pending = true;
    previous.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/calculator-design.md")),
        pending_deliverables: vec![DocsPendingDeliverable {
            target: Utf8PathBuf::from("docs/calculator-design.md"),
            summary: "same-document docs update remains pending".to_string(),
        }],
        ..DocsRouteState::default()
    });
    let history_items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Update `docs/calculator-design.md` only; do not edit calculator.py or test_calculator.py.".to_string(),
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
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Docs write command failed".to_string(),
                output_text: "verification failed: calculator.py appeared in docs command output".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-docs-route-verification-target-authority".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -X utf8 -c \"write docs\"".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: calculator.py should not become the repair target for docs/calculator-design.md".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-docs-route-verification-target-authority".to_string(),
                        failing_labels: vec!["docs semantic check".to_string()],
                        primary_failure: Some("docs command failed".to_string()),
                        evidence: vec![VerificationFailureEvidence {
                            evidence_kind: "verification_failure".to_string(),
                            subtype: Some("generic_verification_failure".to_string()),
                            label: Some("docs semantic check".to_string()),
                            target: Some("calculator.py".to_string()),
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
                            source_refs: vec!["calculator.py".to_string()],
                            test_refs: Vec::new(),
                        }],
                        sibling_obligations: Vec::new(),
                        source_refs: vec!["calculator.py".to_string()],
                        test_refs: Vec::new(),
                    }),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let state = reduce_session_state_from_history_items(&session, &history_items, &[], &previous);
    state.route == TaskRoute::Docs
        && state.process_phase == ProcessPhase::Repair
        && state.completion.route_contract_pending
        && state.completion.verification_pending
        && state.active_targets == vec![Utf8PathBuf::from("docs/calculator-design.md")]
}

#[cfg(test)]
mod tests {
    #[test]
    fn docs_route_single_deliverable_contract_promotes_docs_repair() {
        assert!(
            super::docs_route_single_deliverable_contract_promotes_docs_repair_fixture_passes(),
            "single documentation deliverable should be TaskRoute::Docs / DocsRepair"
        );
    }
}

fn compact_verification_failure_summary(
    command: Option<&str>,
    title: &str,
    summary: &str,
) -> String {
    let labels = extract_verification_failure_labels(summary);
    let detail = compact_verification_failure_detail(summary);
    if !labels.is_empty() {
        return match detail {
            Some(detail) => format!(
                "verification failed: {}; latest detail: {detail}",
                labels.join(", ")
            ),
            None => format!("verification failed: {}", labels.join(", ")),
        };
    }

    if let Some(detail) = detail {
        return format!("verification failed: {detail}");
    }

    let fallback = summary
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| title.trim());
    if fallback.is_empty() {
        command
            .map(|value| format!("verification command failed: {value}"))
            .unwrap_or_else(|| "verification command failed".to_string())
    } else {
        let clipped = clip_text_with_ellipsis(fallback, 180);
        format!("verification failed: {clipped}")
    }
}

fn enrich_verification_failure_summary_with_test_requirement_context(
    compact_summary: &str,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> String {
    let requirement_contexts = test_requirement_contexts_for_unittest_failure(
        compact_summary,
        raw_summary,
        workspace_root,
    );
    let assertion_contexts =
        test_assertion_contexts_for_unittest_failure(compact_summary, raw_summary, workspace_root);
    let mut summary = compact_summary.to_string();
    if !requirement_contexts.is_empty() {
        summary.push_str("; requirement context: ");
        summary.push_str(&requirement_contexts.join("; "));
    }
    if !assertion_contexts.is_empty() {
        summary.push_str("\nsource assertion context:\n");
        summary.push_str(&assertion_contexts.join("\n"));
    }
    summary
}

fn test_requirement_contexts_for_unittest_failure(
    compact_summary: &str,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<String> {
    let labels = extract_verification_failure_labels(compact_summary);
    if labels.is_empty() {
        return Vec::new();
    }
    let test_sources = extract_failure_paths_from_text(raw_summary, workspace_root)
        .into_iter()
        .filter(|path| is_test_focus_target(path))
        .filter_map(|path| read_small_test_context_source(&path, workspace_root))
        .collect::<Vec<_>>();
    if test_sources.is_empty() {
        return Vec::new();
    }

    let mut contexts = Vec::new();
    for label in labels {
        let mut ids = Vec::new();
        for source in &test_sources {
            ids.extend(requirement_ids_for_unittest_label_from_source(
                &label, source,
            ));
        }
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            contexts.push(format!("{} -> {}", label, ids.join(", ")));
        }
        if contexts.len() >= MAX_VERIFICATION_FAILURE_LABELS {
            break;
        }
    }
    contexts.sort();
    contexts.dedup();
    contexts
}

fn read_small_test_context_source(path: &Utf8PathBuf, workspace_root: &Utf8Path) -> Option<String> {
    let full_path = if path.is_absolute() {
        path.clone()
    } else {
        workspace_root.join(path)
    };
    let metadata = fs::metadata(&full_path).ok()?;
    if metadata.len() > 1_000_000 {
        return None;
    }
    fs::read_to_string(full_path).ok()
}

fn test_assertion_contexts_for_unittest_failure(
    compact_summary: &str,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<String> {
    let labels = extract_verification_failure_labels(compact_summary);
    if labels.is_empty() {
        return Vec::new();
    }
    let subjects = local_boolean_assertion_subjects(compact_summary);
    if subjects.is_empty() {
        return Vec::new();
    }
    let test_sources = extract_failure_paths_from_text(raw_summary, workspace_root)
        .into_iter()
        .filter(|path| is_test_focus_target(path))
        .filter_map(|path| read_small_test_context_source(&path, workspace_root))
        .collect::<Vec<_>>();
    if test_sources.is_empty() {
        return Vec::new();
    }

    let mut contexts = Vec::new();
    for label in labels {
        for source in &test_sources {
            let Some(context) =
                local_boolean_assertion_context_for_label(&label, source, &subjects)
            else {
                continue;
            };
            contexts.push(format!("{label}: {}", context.join(" | ")));
            break;
        }
        if contexts.len() >= MAX_VERIFICATION_FAILURE_LABELS {
            break;
        }
    }
    contexts.sort();
    contexts.dedup();
    contexts
}

fn local_boolean_assertion_subjects(summary: &str) -> Vec<String> {
    let mut subjects = Vec::new();
    for line in failure_summary_logical_lines(summary) {
        let trimmed = line.trim();
        for marker in ["self.assertTrue(", "self.assertFalse("] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let end = after
                .find(',')
                .or_else(|| after.find(')'))
                .unwrap_or(after.len());
            let subject = after[..end].trim();
            if is_local_identifier(subject) && !subjects.iter().any(|existing| existing == subject)
            {
                subjects.push(subject.to_string());
            }
        }
    }
    subjects
}

fn local_boolean_assertion_context_for_label(
    label: &str,
    source: &str,
    subjects: &[String],
) -> Option<Vec<String>> {
    let lines = source.lines().collect::<Vec<_>>();
    let method_index = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    })?;
    let method_indent = leading_space_count(lines[method_index]);
    let method_end = lines[method_index + 1..]
        .iter()
        .position(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty()
                && leading_space_count(line) <= method_indent
                && (trimmed.starts_with("def ") || trimmed.starts_with("class "))
        })
        .map(|offset| method_index + 1 + offset)
        .unwrap_or(lines.len());
    let body = &lines[method_index + 1..method_end];
    for subject in subjects {
        let Some(assertion_index) = body.iter().position(|line| {
            let trimmed = line.trim();
            trimmed.contains(&format!("assertTrue({subject}"))
                || trimmed.contains(&format!("assertFalse({subject}"))
        }) else {
            continue;
        };
        let Some(assignment_index) = body[..assertion_index]
            .iter()
            .rposition(|line| line.trim_start().starts_with(&format!("{subject} =")))
        else {
            continue;
        };
        let start = assignment_index.saturating_sub(4);
        let end = (assertion_index + 1).min(body.len());
        let context = body[start..end]
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !context.is_empty() {
            return Some(context);
        }
    }
    None
}

fn leading_space_count(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn failure_summary_logical_lines(summary: &str) -> Vec<&str> {
    summary
        .lines()
        .flat_map(|line| line.split('|'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn is_local_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn requirement_ids_for_unittest_label_from_source(label: &str, source: &str) -> Vec<String> {
    let lines = source.lines().collect::<Vec<_>>();
    let Some(method_index) = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    }) else {
        return Vec::new();
    };
    let class_index = lines[..method_index]
        .iter()
        .rposition(|line| line.trim_start().starts_with("class "))
        .unwrap_or(method_index);
    let context_start = class_index.saturating_sub(3);
    let context_end = (method_index + 10).min(lines.len());
    extract_contract_requirement_ids(&lines[context_start..context_end].join("\n"))
}

fn extract_contract_requirement_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for raw in text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-')) {
        let token = raw.trim_matches(|ch: char| matches!(ch, ':' | '[' | ']' | '`' | '"' | '\''));
        let Some((prefix, number)) = token.split_once('-') else {
            continue;
        };
        if prefix.chars().all(|ch| ch.is_ascii_uppercase())
            && !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
        {
            ids.push(format!("{prefix}-{number}"));
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn compact_verification_failure_detail(summary: &str) -> Option<String> {
    if let Some((_, detail)) = summary.split_once("; latest detail:") {
        let normalized = normalize_verification_failure_detail(detail);
        return (!normalized.is_empty()).then_some(normalized);
    }

    let mut detail_lines = Vec::new();
    let mut capture_source_line = false;
    for raw_line in summary.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            capture_source_line = false;
            continue;
        }
        if trimmed == "Traceback (most recent call last):" {
            push_verification_failure_detail_line(&mut detail_lines, trimmed);
            capture_source_line = false;
            continue;
        }
        if looks_like_contract_requirement_detail_line(trimmed) {
            push_verification_failure_detail_line(&mut detail_lines, trimmed);
            capture_source_line = false;
            continue;
        }
        if trimmed.starts_with("File \"") {
            push_verification_failure_detail_line(&mut detail_lines, trimmed);
            capture_source_line = true;
            continue;
        }
        if capture_source_line {
            if let Some(exception) = normalize_exception_detail_line(trimmed) {
                push_verification_failure_detail_line(&mut detail_lines, &exception);
            } else if looks_like_verification_call_site_line(trimmed) {
                push_verification_failure_detail_line(&mut detail_lines, trimmed);
            }
            capture_source_line = false;
            continue;
        }
        if let Some(exception) = normalize_exception_detail_line(trimmed) {
            push_verification_failure_detail_line(&mut detail_lines, &exception);
            continue;
        }
        if looks_like_verification_call_site_line(trimmed) {
            push_verification_failure_detail_line(&mut detail_lines, trimmed);
        }
    }

    if detail_lines.is_empty() {
        let fallback = summary.lines().map(str::trim).find(|line| {
            !line.is_empty()
                && !line.starts_with("FAIL: ")
                && !line.starts_with("ERROR: ")
                && !line.starts_with("FAILED ")
                && !line.starts_with("Ran ")
                && *line != "----------------------------------------------------------------------"
        })?;
        return Some(normalize_verification_failure_detail(fallback));
    }

    Some(normalize_verification_failure_detail(
        &detail_lines.join(" | "),
    ))
}

fn push_verification_failure_detail_line(lines: &mut Vec<String>, line: &str) {
    if lines.len() >= MAX_VERIFICATION_FAILURE_DETAIL_LINES {
        return;
    }
    let normalized = line.trim();
    if normalized.is_empty() || lines.iter().any(|existing| existing == normalized) {
        return;
    }
    lines.push(normalized.to_string());
}

fn normalize_verification_failure_detail(detail: &str) -> String {
    clip_text_with_ellipsis(
        &detail.trim().replace('\n', " "),
        MAX_VERIFICATION_FAILURE_DETAIL_CHARS,
    )
}

fn normalize_exception_detail_line(line: &str) -> Option<String> {
    let normalized = line.strip_prefix("E   ").unwrap_or(line).trim();
    let (prefix, _) = normalized.split_once(':')?;
    if prefix.ends_with("Error") || prefix.ends_with("Exception") {
        Some(normalized.to_string())
    } else {
        None
    }
}

fn looks_like_contract_requirement_detail_line(line: &str) -> bool {
    let Some((requirement_id, detail)) = line.split_once(':') else {
        return false;
    };
    if detail.trim().is_empty() {
        return false;
    }
    let Some((prefix, number)) = requirement_id.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && !number.is_empty()
        && prefix.chars().all(|ch| ch.is_ascii_uppercase())
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn looks_like_verification_call_site_line(line: &str) -> bool {
    if line.starts_with("FAIL: ")
        || line.starts_with("ERROR: ")
        || line.starts_with("Traceback ")
        || line.starts_with("File \"")
        || line.starts_with("FAILED ")
        || line.starts_with("Ran ")
        || line == "----------------------------------------------------------------------"
        || line.starts_with('^')
    {
        return false;
    }

    line.contains('(') && line.contains(')')
}

fn classify_failure_summary(summary: &str) -> FailureKind {
    let lower = summary.to_ascii_lowercase();
    if lower.contains("invalid tool") || lower.contains("unavailable tool") {
        return FailureKind::InvalidTool;
    }
    if summary_indicates_recoverable_runtime_feedback(&lower)
        || lower.contains("previous response made partial progress but did not use any tools")
        || lower.contains("partial progress no-tool recovery repeated")
        || lower.contains("todo list still had open items")
    {
        return FailureKind::CompletionDrift;
    }
    if lower.contains("verification")
        || lower.contains("unittest")
        || lower.contains("integration test")
    {
        return FailureKind::VerificationFailed;
    }
    if summary_indicates_patch_mismatch(&lower) {
        return FailureKind::PatchMismatch;
    }
    if lower.contains("maximum steps")
        || lower.contains("completion")
        || lower.contains("todo list still had open items")
    {
        return FailureKind::CompletionDrift;
    }
    FailureKind::ToolExecution
}

pub(crate) fn runtime_feedback_summary_preserves_completion_authority(summary: &str) -> bool {
    matches!(
        classify_failure_summary(summary),
        FailureKind::CompletionDrift
    )
}

fn summary_indicates_recoverable_runtime_feedback(lower_summary: &str) -> bool {
    lower_summary.contains("recoverable_runtime_feedback")
        || lower_summary
            .contains("previous response did not use any tools while typed work remains")
        || lower_summary.contains("runtime requires another concrete tool action")
        || lower_summary
            .contains("runtime requires a call through one of the currently allowed tools")
}

fn summary_indicates_patch_mismatch(lower_summary: &str) -> bool {
    lower_summary.contains("patch mismatch")
        || lower_summary.contains("context mismatch")
        || lower_summary.contains("tool edit error")
        || lower_summary.contains("failed to find expected lines")
        || lower_summary.contains("apply_patch failed")
        || lower_summary.contains("patch application failed")
}

fn task_route_label(route: TaskRoute) -> &'static str {
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

#[cfg(test)]
mod message_user_verification_target_tests {
    #[test]
    fn message_user_protected_reference_filters_verification_targets() {
        assert!(
            super::message_user_protected_reference_filters_verification_targets_fixture_passes()
        );
    }

    #[test]
    fn message_user_structured_document_progress() {
        assert!(super::message_user_structured_document_progress_fixture_passes());
    }

    #[test]
    fn requested_work_missing_todo_graph_stays_authoring_authority() {
        assert!(super::requested_work_missing_todo_graph_stays_authoring_authority());
    }
}

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use serde_json::{Value, json};

use crate::agent::completion_guard::completion_workspace_blocked_reason;
use crate::agent::language_evidence::{
    ArtifactRole, LanguageLocalBindingContradiction,
    classify_artifact_target as classify_language_artifact_target,
    language_failure_assertion_contexts_from_sources, language_failure_labels_from_summary,
    language_failure_paths_from_summary, language_failure_requirement_contexts_from_sources,
    language_generated_test_local_binding_contradictions, language_source_targets_from_text,
    language_verification_failure_summary_evidence, language_verification_repair_authority_target,
    language_verification_runner_byproduct_or_dependency, language_verification_target_candidates,
};
use crate::agent::prompt::{
    extract_protected_artifact_targets, extract_requested_artifact_targets,
    looks_like_structured_document_work, requested_work_contract_from_instruction_text,
    same_document_update_alias_requested, staged_task_artifact_targets_from_text,
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
    MessageRole, ProcessPhase, SessionStateSnapshot, TaskRoute, TodoItem,
    VerificationFailureCluster, VerificationFailureEvidence,
};
use crate::tool::ToolName;
use crate::tool::truncate::clip_text_with_ellipsis;

const MAX_VERIFICATION_FAILURE_LABELS: usize = 8;
const MAX_VERIFICATION_FAILURE_DETAIL_LINES: usize = 28;
const MAX_VERIFICATION_FAILURE_DETAIL_CHARS: usize = 2600;
const STATE_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const STATE_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateAuthorityOwner {
    RequestedWorkAuthoring,
    DocsRepair,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateAuthorityDecision {
    owner: StateAuthorityOwner,
    active_work: ActiveWorkContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateAuthorityCandidate {
    owner: StateAuthorityOwner,
    active_work: ActiveWorkContract,
    precedence: u8,
    invariant_ref: &'static str,
}

impl StateAuthorityCandidate {
    fn into_decision(self) -> StateAuthorityDecision {
        StateAuthorityDecision {
            owner: self.owner,
            active_work: self.active_work,
        }
    }
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
    if let Some(typed_evidence) = latest_typed_verification_failure_context(session, history_items)
    {
        state = apply_verification_failure_authority(session, history_items, state, typed_evidence);
    }
    materialize_state_authority_projection(session, history_items, state)
}

fn apply_verification_failure_authority(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
    typed_evidence: TypedVerificationFailureEvidence,
) -> SessionStateSnapshot {
    let post_failure_written_targets = observed_written_targets_since_latest_verification_failure(
        history_items,
        session.cwd.as_path(),
    );
    let post_failure_content_progress = content_changing_progress_since_latest_verification_failure(
        history_items,
        session.cwd.as_path(),
    );

    let repair_authority_targets =
        repair_progress_authority_targets(&typed_evidence, session.cwd.as_path());
    let docs_route_owns_failure =
        docs_route_failure_matches_current_requested_docs_contract(session, &state, history_items);
    let verification_targets_override_docs_route =
        !repair_authority_targets.is_empty() && !docs_route_owns_failure;
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
    let mut remaining_failure_targets = if repair_progress_observed {
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
    if !repair_progress_observed && docs_route_owns_failure {
        remaining_failure_targets = docs_route_pending_repair_targets(state.docs_route.as_ref());
    } else if remaining_failure_targets.is_empty()
        && !repair_progress_observed
        && state.completion.route_contract_pending
        && state.docs_route.is_some()
    {
        remaining_failure_targets = docs_route_pending_repair_targets(state.docs_route.as_ref());
    }
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
    if verification_targets_override_docs_route {
        state.completion.route_contract_pending = false;
    }
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

fn docs_route_failure_matches_current_requested_docs_contract(
    session: &SessionRecord,
    state: &SessionStateSnapshot,
    history_items: &[HistoryItem],
) -> bool {
    if !(state.completion.route_contract_pending && state.docs_route.is_some()) {
        return false;
    }
    let Some(latest_user) = latest_user_text_from_history_items(history_items) else {
        return false;
    };
    let docs_targets = docs_route_pending_deliverables_from_state(state.docs_route.as_ref())
        .into_iter()
        .map(|item| canonical_target_key(item.target.as_str()))
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    if docs_targets.is_empty() {
        return false;
    }
    let explicit_required_commands = explicit_required_verification_commands_from_history_items(
        session.cwd.as_path(),
        Some(latest_user.as_str()),
    );
    let requested_work = requested_work_discipline_from_history_items(
        session.cwd.as_path(),
        history_items,
        Some(latest_user.as_str()),
        &explicit_required_commands,
        None,
    );
    let requested_targets = requested_work
        .required_targets
        .iter()
        .chain(requested_work.pending_targets.iter())
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    let mentioned_targets = extract_requested_artifact_targets(&latest_user)
        .into_iter()
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    let protected_or_reference_targets = requested_work
        .reference_inputs
        .iter()
        .chain(requested_work.protected_targets.iter())
        .map(|target| canonical_target_key(target.as_str()))
        .chain(
            extract_protected_artifact_targets(&latest_user)
                .into_iter()
                .map(|target| canonical_target_key(target.as_str())),
        )
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    if mentioned_targets.iter().any(|target| {
        !docs_targets.contains(target) && !protected_or_reference_targets.contains(target)
    }) {
        return false;
    }
    if requested_targets.is_empty() {
        return docs_route_authority_matches_active_targets(state)
            && mentioned_targets
                .iter()
                .all(|target| docs_targets.contains(target));
    }
    requested_targets
        .iter()
        .all(|target| docs_targets.contains(target))
        && requested_targets
            .iter()
            .any(|target| docs_targets.contains(target))
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
        "verify-workflow --behavior draft".to_string(),
        "verify-workflow --behavior review".to_string(),
    ];
    merge_required_commands(
        &mut required,
        &[
            "verify-workflow --behavior draft".to_string(),
            "verify-workflow --behavior review".to_string(),
            "verify-workflow --behavior draft".to_string(),
        ],
    );

    let run = VerificationRunResult {
        command: "verify-workflow --behavior draft".to_string(),
        status: VerificationRunStatus::Passed,
        exit_code: Some(0),
        timed_out: false,
        output_summary: "workflow behavior draft verified".to_string(),
        failure_cluster: None,
        satisfies_command_identities: Vec::new(),
        artifact_refs: Vec::new(),
        requirement_refs: Vec::new(),
    };
    let run_keys = verification_run_satisfaction_keys(&run);

    required.len() == 2
        && run_keys.contains("verify-workflow --behavior draft")
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
                verification_run,
                ..
            } => {
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
            retain_implementation_handoff_remaining_without_observed_target_progress(
                &mut handoff.remaining,
                &observed_written_targets,
                workspace_root,
            );
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

fn retain_implementation_handoff_remaining_without_observed_target_progress(
    remaining: &mut Vec<String>,
    observed_written_targets: &BTreeSet<Utf8PathBuf>,
    workspace_root: &Utf8Path,
) {
    if observed_written_targets.is_empty() {
        return;
    }
    let observed_target_keys = observed_written_targets
        .iter()
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|key| !key.is_empty())
        .collect::<BTreeSet<_>>();
    remaining.retain(|item| {
        !implementation_handoff_remaining_item_satisfied_by_observed_targets(
            item,
            &observed_target_keys,
            workspace_root,
        )
    });
}

fn implementation_handoff_remaining_item_satisfied_by_observed_targets(
    item: &str,
    observed_target_keys: &BTreeSet<String>,
    workspace_root: &Utf8Path,
) -> bool {
    let item_target_keys = implementation_handoff_remaining_item_target_keys(item, workspace_root);
    !item_target_keys.is_empty()
        && item_target_keys
            .iter()
            .all(|key| observed_target_keys.contains(key))
}

fn implementation_handoff_remaining_item_target_keys(
    item: &str,
    workspace_root: &Utf8Path,
) -> BTreeSet<String> {
    implementation_handoff_remaining_target_tokens(item)
        .into_iter()
        .filter_map(|target| normalize_target_path(&target, workspace_root))
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|key| !key.is_empty())
        .collect()
}

fn implementation_handoff_remaining_target_tokens(item: &str) -> Vec<String> {
    item.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '='
            )
    })
    .filter_map(implementation_handoff_remaining_target_token)
    .collect()
}

fn implementation_handoff_remaining_target_token(raw: &str) -> Option<String> {
    let token = raw.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
        )
    });
    let token = if let Some((prefix, suffix)) = token.rsplit_once(':') {
        if prefix.len() != 1 && (suffix.contains('/') || suffix.contains('\\')) {
            suffix
        } else {
            token
        }
    } else {
        token
    }
    .trim_end_matches(':');
    let has_path_shape = token.contains('/') || token.contains('\\') || token.contains('.');
    (!token.is_empty() && has_path_shape).then(|| token.to_string())
}

pub(crate) fn state_handoff_remaining_exact_target_identity_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let Some(observed_target) = normalize_target_path("src/workflow.rs", workspace_root) else {
        return false;
    };
    let observed_targets = [observed_target].into_iter().collect::<BTreeSet<_>>();
    let satisfied_item = "finish src/workflow.rs";
    let sibling_item = "finish sibling/src/workflow.rs";
    let suffix_item = "finish src/workflow.rs.backup";
    let multi_target_item = "finish src/workflow.rs and tests/workflow.contract";
    let untargeted_item = "finish workflow source without target token";
    let mut remaining = vec![
        satisfied_item.to_string(),
        sibling_item.to_string(),
        suffix_item.to_string(),
        multi_target_item.to_string(),
        untargeted_item.to_string(),
    ];

    retain_implementation_handoff_remaining_without_observed_target_progress(
        &mut remaining,
        &observed_targets,
        workspace_root,
    );

    !remaining.iter().any(|item| item == satisfied_item)
        && remaining.iter().any(|item| item == sibling_item)
        && remaining.iter().any(|item| item == suffix_item)
        && remaining.iter().any(|item| item == multi_target_item)
        && remaining.iter().any(|item| item == untargeted_item)
        && implementation_handoff_remaining_item_satisfied_by_observed_targets(
            "finish C:/workspace/project/src/workflow.rs",
            &observed_targets
                .iter()
                .map(|target| canonical_target_key(target.as_str()))
                .collect::<BTreeSet<_>>(),
            workspace_root,
        )
}

fn history_items_in_sequence(history_items: &[HistoryItem]) -> Vec<&HistoryItem> {
    let mut ordered = history_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| item.sequence_no);
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
    item.sequence_no
}

pub(crate) fn state_history_item_sequence_primary_order_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 400,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "old request".to_string(),
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
            created_at_ms: 500,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "old error evidence".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 100,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "latest request by sequence".to_string(),
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
            sequence_no: 4,
            created_at_ms: 200,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "latest request evidence".to_string(),
            },
        },
    ];

    let ordered_sequences = history_items_in_sequence(&items)
        .into_iter()
        .map(|item| item.sequence_no)
        .collect::<Vec<_>>();
    let latest_window_sequences = history_items_since_latest_user_turn(&items)
        .into_iter()
        .map(|item| item.sequence_no)
        .collect::<Vec<_>>();

    ordered_sequences == vec![1, 2, 3, 4]
        && latest_user_turn_sequence(&items) == Some(3)
        && latest_window_sequences == vec![3, 4]
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
    if state.completion.route_contract_pending
        && state.docs_route.is_some()
        && docs_route_authority_matches_active_targets(state)
    {
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

    None
}

fn state_authority_decision_for_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
) -> Option<StateAuthorityDecision> {
    state_authority_candidates_for_history_items(session, history_items, state)
        .into_iter()
        .min_by_key(|candidate| (candidate.precedence, candidate.invariant_ref))
        .map(StateAuthorityCandidate::into_decision)
}

fn state_authority_candidates_for_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
) -> Vec<StateAuthorityCandidate> {
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
    let mut candidates = Vec::new();

    if let Some(snapshot) = structured_document_summary_snapshot_from_history_items(
        session.cwd.as_path(),
        history_items,
        latest_user.as_deref(),
    ) {
        if !snapshot.missing_files.is_empty() {
            candidates.push(StateAuthorityCandidate {
                owner: StateAuthorityOwner::RequestedWorkAuthoring,
                active_work: ActiveWorkContract::RequestedWorkAuthoring {
                    pending_targets: vec![Utf8PathBuf::from(snapshot.output_target)],
                    verification_commands: Vec::new(),
                },
                precedence: 10,
                invariant_ref: "structured_document_summary_remaining_sources",
            });
        }
    }

    if !state.completion.verification_pending
        && state.failure.is_none()
        && !requested_work.pending_targets.is_empty()
        && !requested_work_targets_are_docs_route_deliverables(
            &requested_work.pending_targets,
            state,
        )
    {
        candidates.push(StateAuthorityCandidate {
            owner: StateAuthorityOwner::RequestedWorkAuthoring,
            active_work: ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets: requested_work.pending_targets.clone(),
                verification_commands: requested_work.verification_commands.clone(),
            },
            precedence: 20,
            invariant_ref: "latest_requested_work_authoring",
        });
    }

    if state.completion.route_contract_pending
        && state.docs_route.is_some()
        && docs_route_authority_matches_active_targets(state)
    {
        if let Some(active_work) = state_native_active_work_contract(state) {
            candidates.push(StateAuthorityCandidate {
                owner: StateAuthorityOwner::DocsRepair,
                active_work,
                precedence: 30,
                invariant_ref: "docs_route_active_target_authority",
            });
        }
    }

    if state.completion.verification_pending && matches!(state.process_phase, ProcessPhase::Verify)
    {
        if let Some(active_work) = state_native_active_work_contract(state) {
            candidates.push(StateAuthorityCandidate {
                owner: StateAuthorityOwner::Verification,
                active_work,
                precedence: 40,
                invariant_ref: "verification_pending_authority",
            });
        }
    }

    if state.completion.verification_pending
        && matches!(state.process_phase, ProcessPhase::Repair)
        && matches!(
            state.failure.as_ref().map(|failure| failure.kind),
            Some(FailureKind::VerificationFailed | FailureKind::PatchMismatch)
        )
    {
        candidates.push(StateAuthorityCandidate {
            owner: StateAuthorityOwner::Verification,
            active_work: ActiveWorkContract::Verification {
                commands: verification_commands_for_active_repair(state, &requested_work),
                failing_labels: state.verification.failing_labels.clone(),
                repair_required: true,
                targets: contract_reconciled_verification_repair_targets(
                    state,
                    session.cwd.as_path(),
                )
                .or_else(|| verification_repair_targets_from_state(state))
                .unwrap_or_else(|| state.active_targets.clone()),
            },
            precedence: 50,
            invariant_ref: "verification_repair_authority",
        });
    }

    if state.completion.verification_pending {
        if let Some(active_work) = state_native_active_work_contract(state) {
            candidates.push(StateAuthorityCandidate {
                owner: StateAuthorityOwner::Verification,
                active_work,
                precedence: 60,
                invariant_ref: "verification_pending_fallback",
            });
        }
    }

    if state.completion.route_contract_pending && state.docs_route.is_some() {
        if let Some(active_work) = state_native_active_work_contract(state) {
            candidates.push(StateAuthorityCandidate {
                owner: StateAuthorityOwner::DocsRepair,
                active_work,
                precedence: 70,
                invariant_ref: "docs_route_pending_fallback",
            });
        }
    }

    if !requested_work.pending_targets.is_empty() {
        candidates.push(StateAuthorityCandidate {
            owner: StateAuthorityOwner::RequestedWorkAuthoring,
            active_work: ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets: requested_work.pending_targets.clone(),
                verification_commands: requested_work.verification_commands.clone(),
            },
            precedence: 80,
            invariant_ref: "requested_work_authoring_fallback",
        });
    }

    if !requested_work.required_targets.is_empty()
        && !requested_work.verification_commands.is_empty()
        && !requested_work_verification_passed(session, history_items)
    {
        candidates.push(StateAuthorityCandidate {
            owner: StateAuthorityOwner::Verification,
            active_work: ActiveWorkContract::Verification {
                commands: requested_work.verification_commands.clone(),
                failing_labels: Vec::new(),
                repair_required: false,
                targets: Vec::new(),
            },
            precedence: 90,
            invariant_ref: "requested_work_verification_obligation",
        });
    }

    if let Some(active_work) = state_native_active_work_contract(state) {
        let owner = match active_work {
            ActiveWorkContract::RequestedWorkAuthoring { .. } => {
                StateAuthorityOwner::RequestedWorkAuthoring
            }
            ActiveWorkContract::DocsRepair { .. } => StateAuthorityOwner::DocsRepair,
            ActiveWorkContract::Verification { .. } => StateAuthorityOwner::Verification,
        };
        candidates.push(StateAuthorityCandidate {
            owner,
            active_work,
            precedence: 100,
            invariant_ref: "state_native_projection_fallback",
        });
    }

    candidates
}

fn requested_work_targets_are_docs_route_deliverables(
    targets: &[Utf8PathBuf],
    state: &SessionStateSnapshot,
) -> bool {
    if targets.is_empty() {
        return false;
    }
    let docs_targets = docs_route_pending_deliverables_from_state(state.docs_route.as_ref())
        .into_iter()
        .map(|item| canonical_target_key(item.target.as_str()))
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    !docs_targets.is_empty()
        && targets
            .iter()
            .all(|target| docs_targets.contains(&canonical_target_key(target.as_str())))
}

fn materialize_state_authority_projection(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    mut state: SessionStateSnapshot,
) -> SessionStateSnapshot {
    let Some(decision) = state_authority_decision_for_history_items(session, history_items, &state)
    else {
        return state;
    };
    let StateAuthorityDecision { owner, active_work } = decision;
    match active_work {
        ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets,
            verification_commands,
        } => {
            debug_assert_eq!(owner, StateAuthorityOwner::RequestedWorkAuthoring);
            let active_work = ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets: pending_targets.clone(),
                verification_commands: verification_commands.clone(),
            };
            state.route = TaskRoute::Code;
            state.process_phase = ProcessPhase::Author;
            state.active_targets = pending_targets.clone();
            state.verification.required_commands = verification_commands;
            state.completion.open_work_count = pending_targets.len().max(1);
            state.completion.closeout_ready = false;
            state.completion.verification_pending = false;
            state.completion.route_contract_pending = false;
            state.completion.route_contract_summary = None;
            state.docs_route = None;
            state.completion.blocked_reason =
                selected_owner_blocked_reason(state.completion.blocked_reason.take(), &active_work);
        }
        ActiveWorkContract::DocsRepair {
            deliverable,
            pending_deliverables,
            pending_summary,
            route_contract_satisfied: _,
        } => {
            debug_assert_eq!(owner, StateAuthorityOwner::DocsRepair);
            state.route = TaskRoute::Docs;
            state.process_phase = if matches!(
                state.process_phase,
                ProcessPhase::Author | ProcessPhase::Repair
            ) {
                state.process_phase
            } else {
                ProcessPhase::Author
            };
            state.active_targets = if pending_deliverables.is_empty() {
                deliverable.into_iter().collect()
            } else {
                pending_deliverables
                    .iter()
                    .map(|item| item.target.clone())
                    .collect()
            };
            state.completion.open_work_count = state.active_targets.len().max(1);
            state.completion.closeout_ready = false;
            state.completion.route_contract_pending = true;
            state.completion.route_contract_summary = Some(pending_summary.clone());
            let active_work = ActiveWorkContract::DocsRepair {
                deliverable: state.active_targets.first().cloned(),
                pending_deliverables,
                pending_summary,
                route_contract_satisfied: false,
            };
            state.completion.blocked_reason =
                selected_owner_blocked_reason(state.completion.blocked_reason.take(), &active_work);
        }
        ActiveWorkContract::Verification {
            commands,
            failing_labels,
            repair_required,
            targets,
        } => {
            debug_assert_eq!(owner, StateAuthorityOwner::Verification);
            let current_docs_route_context = state.route == TaskRoute::Docs
                && state.docs_route.is_some()
                && (!state.completion.route_contract_pending
                    || docs_route_failure_matches_current_requested_docs_contract(
                        session,
                        &state,
                        history_items,
                    ));
            if !current_docs_route_context
                && !docs_route_failure_matches_current_requested_docs_contract(
                    session,
                    &state,
                    history_items,
                )
            {
                state.route = TaskRoute::Code;
                state.completion.route_contract_pending = false;
                state.completion.route_contract_summary = None;
                state.docs_route = None;
            }
            state.process_phase = if repair_required {
                ProcessPhase::Repair
            } else {
                ProcessPhase::Verify
            };
            let active_work = ActiveWorkContract::Verification {
                commands: commands.clone(),
                failing_labels: failing_labels.clone(),
                repair_required,
                targets: targets.clone(),
            };
            state.active_targets = targets;
            merge_required_commands(&mut state.verification.required_commands, &commands);
            if !failing_labels.is_empty() {
                state.verification.failing_labels = failing_labels;
            }
            state.completion.open_work_count = 0;
            state.completion.closeout_ready = false;
            state.completion.verification_pending = true;
            state.completion.blocked_reason =
                selected_owner_blocked_reason(state.completion.blocked_reason.take(), &active_work);
        }
    }
    state
}

fn selected_owner_blocked_reason(
    current: Option<String>,
    active_work: &ActiveWorkContract,
) -> Option<String> {
    if current
        .as_deref()
        .is_some_and(|reason| blocked_reason_matches_selected_owner(reason, active_work))
    {
        current
    } else {
        Some(active_work.summary())
    }
}

fn blocked_reason_matches_selected_owner(reason: &str, active_work: &ActiveWorkContract) -> bool {
    let reason_lower = reason.to_ascii_lowercase();
    let active_target_keys = active_work
        .targets()
        .iter()
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|target| !target.is_empty())
        .collect::<BTreeSet<_>>();
    let reason_target_keys = blocked_reason_target_keys(reason);
    if !reason_target_keys.is_empty() {
        if !active_target_keys.is_empty()
            && reason_target_keys
                .iter()
                .all(|target| active_target_keys.contains(target))
        {
            return true;
        }
    }
    if !active_target_keys.is_empty()
        && active_target_keys
            .iter()
            .any(|target| reason_lower == target.to_ascii_lowercase())
    {
        return true;
    }
    match active_work {
        ActiveWorkContract::RequestedWorkAuthoring { .. } => {
            if reason_lower.contains("structured document summary") {
                return true;
            }
            reason_target_keys.is_empty()
                && (reason_lower.contains("requested deliverable")
                    || reason_lower.contains("requested-work"))
        }
        ActiveWorkContract::DocsRepair {
            pending_summary, ..
        } => {
            reason_lower.contains("docs route")
                || reason_lower.contains("docs contract")
                || reason_lower.contains("same-document docs")
                || !pending_summary.trim().is_empty()
                    && reason_lower.contains(&pending_summary.to_ascii_lowercase())
        }
        ActiveWorkContract::Verification {
            failing_labels,
            commands,
            repair_required,
            ..
        } => {
            reason_lower.contains("verification")
                || (*repair_required && reason_lower.contains("repair"))
                || failing_labels
                    .iter()
                    .any(|label| reason_lower.contains(&label.to_ascii_lowercase()))
                || commands
                    .iter()
                    .any(|command| reason_lower.contains(&command.to_ascii_lowercase()))
        }
    }
}

fn blocked_reason_target_keys(reason: &str) -> BTreeSet<String> {
    implementation_handoff_remaining_target_tokens(reason)
        .into_iter()
        .filter_map(|target| normalize_target_path(&target, Utf8Path::new("")))
        .map(|target| canonical_target_key(target.as_str()))
        .filter(|key| !key.is_empty())
        .collect()
}

pub(crate) fn state_blocked_reason_exact_target_identity_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        verification_commands: Vec::new(),
    };
    let stale_suffix_reason = "requested-work remains for sibling/src/workflow.rs";
    let exact_reason = "requested-work remains for src/workflow.rs";
    let untargeted_owner_reason = "requested-work remains";
    let structured_dependency_reason =
        "structured document summary still needs docs/workflow-source.md";

    let stale_result =
        selected_owner_blocked_reason(Some(stale_suffix_reason.to_string()), &active_work);
    let exact_result = selected_owner_blocked_reason(Some(exact_reason.to_string()), &active_work);
    let untargeted_result =
        selected_owner_blocked_reason(Some(untargeted_owner_reason.to_string()), &active_work);
    let structured_dependency_result = selected_owner_blocked_reason(
        Some(structured_dependency_reason.to_string()),
        &ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("docs/workflow-summary.md")],
            verification_commands: Vec::new(),
        },
    );

    stale_result.as_deref() != Some(stale_suffix_reason)
        && stale_result
            .as_deref()
            .is_some_and(|reason| reason.contains("src/workflow.rs"))
        && exact_result.as_deref() == Some(exact_reason)
        && untargeted_result.as_deref() == Some(untargeted_owner_reason)
        && structured_dependency_result.as_deref() == Some(structured_dependency_reason)
}

fn docs_route_authority_matches_active_targets(state: &SessionStateSnapshot) -> bool {
    let docs_targets = docs_route_pending_deliverables_from_state(state.docs_route.as_ref())
        .into_iter()
        .map(|item| canonical_target_key(item.target.as_str()))
        .collect::<BTreeSet<_>>();
    if docs_targets.is_empty() {
        return false;
    }
    !state.active_targets.is_empty()
        && state
            .active_targets
            .iter()
            .all(|target| docs_targets.contains(&canonical_target_key(target.as_str())))
}

pub(crate) fn active_work_contract_for_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    _todos: &[TodoItem],
) -> Option<ActiveWorkContract> {
    state_authority_decision_for_history_items(session, history_items, state)
        .map(|decision| decision.active_work)
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
        if !is_verification_repair_authority_target(&normalized_path) {
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
        let source_targets = source_owned_active_work_repair_targets_from_generated_test_evidence(
            &targets,
            failure_cluster,
        );
        if !source_targets.is_empty() {
            return Some(source_targets);
        }
    }
    Some(targets)
}

fn source_owned_active_work_repair_targets_from_generated_test_evidence(
    targets: &[Utf8PathBuf],
    cluster: Option<&VerificationFailureCluster>,
) -> Vec<Utf8PathBuf> {
    let mut source_targets =
        verification_cluster_source_call_site_targets(cluster, Utf8Path::new(""));
    source_targets.extend(
        targets
            .iter()
            .filter(|target| is_code_or_test_target(target) && !is_test_focus_target(target))
            .cloned(),
    );
    if source_targets.is_empty() {
        source_targets.extend(
            targets
                .iter()
                .filter(|target| is_test_focus_target(target))
                .filter_map(|target| source_path_for_generated_test_target(target.as_str()))
                .filter_map(|source| normalize_target_path(&source, Utf8Path::new(""))),
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
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.contract"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: workflow.advance is missing".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![
            Utf8PathBuf::from("src/workflow.rs"),
            Utf8PathBuf::from("tests/workflow.contract"),
        ],
    });
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-diagnostic-scalar-targets".to_string(),
        failing_labels: vec!["workflow behavior contract".to_string()],
        primary_failure: Some(
            "workflow behavior contract reports missing advance operation".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("workflow behavior contract".to_string()),
            target: Some(" 0".to_string()),
            symbol: Some("workflow.advance".to_string()),
            call_site: Some("workflow.advance(\"draft\")".to_string()),
            exception: Some("MissingOperation".to_string()),
            expected: Some("review step".to_string()),
            observed: Some("workflow.advance is missing".to_string()),
            public_state_assertions: vec!["workflow.advance(\"draft\")".to_string()],
            public_missing_attributes: vec!["workflow.advance".to_string()],
            evidence_markers: vec!["public_class_attribute_mismatch".to_string()],
            sibling_obligations: vec!["`workflow.advance` is missing".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec![" 0".to_string(), "workflow draft step".to_string()],
            test_refs: vec!["tests/workflow.contract".to_string()],
        }],
        sibling_obligations: vec!["`workflow.advance` is missing".to_string()],
        source_refs: vec![" 0".to_string(), "workflow draft step".to_string()],
        test_refs: vec!["tests/workflow.contract".to_string()],
    });
    let Some(targets) = verification_repair_targets_from_state(&state) else {
        return false;
    };
    targets == vec![Utf8PathBuf::from("src/workflow.rs")]
}

pub(crate) fn verification_repair_targets_from_state_uses_common_repair_authority_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("package.json"),
        Utf8PathBuf::from("target/cache/generated.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: package script contract mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-common-repair-authority-targets".to_string(),
        failing_labels: vec!["build script contract".to_string()],
        primary_failure: Some("npm test failed after package script config change".to_string()),
        evidence: vec![
            VerificationFailureEvidence {
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
            },
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("generic_verification_failure".to_string()),
                label: Some("runner byproduct diagnostic".to_string()),
                target: Some("target/cache/generated.ts".to_string()),
                symbol: None,
                call_site: None,
                exception: None,
                expected: None,
                observed: Some("runner cache noted generated file".to_string()),
                public_state_assertions: Vec::new(),
                public_missing_attributes: Vec::new(),
                evidence_markers: Vec::new(),
                sibling_obligations: Vec::new(),
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: Vec::new(),
            },
        ],
        sibling_obligations: Vec::new(),
        source_refs: vec!["package.json".to_string()],
        test_refs: Vec::new(),
    });

    let Some(targets) = verification_repair_targets_from_state(&state) else {
        return false;
    };
    targets == vec![Utf8PathBuf::from("package.json")]
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.contract")];
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-workflow --behavior repair".to_string());
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public stderr assertion mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.verification.failing_labels = vec!["workflow stderr contract".to_string()];
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-public-output-source-target".to_string(),
        failing_labels: vec!["workflow stderr contract".to_string()],
        primary_failure: Some("Command: verify-workflow --behavior repair".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("workflow stderr contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: Some("workflow output stream assertion".to_string()),
            exception: None,
            expected: Some("expected workflow stderr token".to_string()),
            observed: Some("stderr `unmatched workflow output`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_output_stream:stderr".to_string(),
                "source_public_behavior_assertion".to_string(),
            ],
            sibling_obligations: vec!["stderr contains expected token".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.contract".to_string()],
        }],
        sibling_obligations: vec!["stderr contains expected token".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.contract".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("src/workflow.rs")]
    ) && diagnostic.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && diagnostic
            .active_work_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("`src/workflow.rs`"))
        && diagnostic
            .repair_lane
            .as_ref()
            .is_some_and(|lane| lane.required_target.as_deref() == Some("src/workflow.rs"))
}

pub(crate) fn state_reducer_ignores_projection_cache_items_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "projection cache state fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut projected_state = SessionStateSnapshot::default();
    projected_state.active_targets = vec![Utf8PathBuf::from("stale.rs")];
    projected_state.completion.open_work_count = 1;
    projected_state.completion.blocked_reason =
        Some("projection-only stale target stale.rs".to_string());
    let projection = crate::session::TurnDecisionDiagnostic {
        route: "code".to_string(),
        process_phase: "author".to_string(),
        active_work_kind: Some("projection-only".to_string()),
        active_work_summary: Some("projection-only stale target stale.rs".to_string()),
        active_targets: vec![Utf8PathBuf::from("stale.rs")],
        verification_pending: false,
        closeout_ready: false,
        required_verification_commands: Vec::new(),
        policy_targets: vec!["stale.rs".to_string()],
        allowed_tools: vec!["write".to_string()],
        tool_choice: Some("auto".to_string()),
        warnings: Vec::new(),
        repair_lane: None,
    };
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
                    text: "Create active.rs".to_string(),
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
            payload: HistoryItemPayload::SessionState {
                state: projected_state,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::StateProjection { projection },
        },
    ];
    let reduced = reduce_session_state_from_history_items(
        &session,
        &history_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let active = active_work_contract_for_history_items(&session, &history_items, &reduced, &[]);

    reduced.active_targets == vec![Utf8PathBuf::from("active.rs")]
        && reduced.completion.open_work_count == 1
        && reduced
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("active.rs") && !reason.contains("stale.rs"))
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring { pending_targets, .. })
                if pending_targets == vec![Utf8PathBuf::from("active.rs")]
        )
}

pub(crate) fn state_authority_projection_replaces_stale_blocked_reason_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "single owner blocked reason fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot {
        route: TaskRoute::Docs,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("docs/old-design.md")],
        docs_route: Some(DocsRouteState {
            pending_deliverables: vec![DocsPendingDeliverable {
                target: Utf8PathBuf::from("docs/old-design.md"),
                summary: "stale docs deliverable".to_string(),
            }],
            ..DocsRouteState::default()
        }),
        ..SessionStateSnapshot::default()
    };
    previous.completion.route_contract_pending = true;
    previous.completion.route_contract_summary =
        Some("stale docs route contract summary".to_string());
    previous.completion.blocked_reason =
        Some("stale docs blocked reason for docs/old-design.md".to_string());

    let history_items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Create active.rs".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];

    let reduced = reduce_session_state_from_history_items(&session, &history_items, &[], &previous);
    reduced.route == TaskRoute::Code
        && reduced.process_phase == ProcessPhase::Author
        && reduced.active_targets == vec![Utf8PathBuf::from("active.rs")]
        && !reduced.completion.route_contract_pending
        && reduced.completion.route_contract_summary.is_none()
        && reduced.docs_route.is_none()
        && reduced
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("active.rs") && !reason.contains("old-design"))
}

fn target_is_test_like(target: &str) -> bool {
    classify_language_artifact_target(target).role == ArtifactRole::Test
}

fn source_path_for_generated_test_target(target: &str) -> Option<String> {
    let spec = classify_language_artifact_target(target);
    (spec.role == ArtifactRole::Test)
        .then_some(spec.source_path)
        .flatten()
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
                    let summary = enrich_verification_failure_summary_with_language_context(
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Repair src/workflow.rs according to docs/workflow-contract.md, but do not change docs/workflow-contract.md.".to_string(),
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
                    command: "verify-workflow --behavior repair".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "public behavior contract mismatch".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-message-user-protected-target".to_string(),
                        failing_labels: vec!["workflow public behavior".to_string()],
                        primary_failure: Some("public behavior mismatch".to_string()),
                        evidence: vec![VerificationFailureEvidence {
                            evidence_kind: "verification_failure".to_string(),
                            subtype: Some("public_state_assertion_mismatch".to_string()),
                            label: Some("workflow public behavior".to_string()),
                            target: Some("src/workflow.rs".to_string()),
                            symbol: None,
                            call_site: Some("workflow.advance()".to_string()),
                            exception: None,
                            expected: Some("contract behavior".to_string()),
                            observed: Some("wrong behavior".to_string()),
                            public_state_assertions: vec!["workflow.advance()".to_string()],
                            public_missing_attributes: Vec::new(),
                            evidence_markers: vec!["source_public_behavior_assertion".to_string()],
                            sibling_obligations: Vec::new(),
                            requirement_refs: Vec::new(),
                            source_refs: vec![
                                "src/workflow.rs".to_string(),
                                "docs/workflow-contract.md".to_string(),
                            ],
                            test_refs: vec!["tests/workflow.contract".to_string()],
                        }],
                        sibling_obligations: Vec::new(),
                        source_refs: vec![
                            "src/workflow.rs".to_string(),
                            "docs/workflow-contract.md".to_string(),
                        ],
                        test_refs: vec!["tests/workflow.contract".to_string()],
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
        .any(|target| target.as_str() == "src/workflow.rs")
        && evidence
            .failure
            .targets
            .iter()
            .all(|target| target.as_str() != "docs/workflow-contract.md")
        && evidence.required_commands == vec!["verify-workflow --behavior repair".to_string()]
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
    .filter(|target| is_verification_repair_authority_target(target.as_path()))
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
    if lower.contains("interactive stdin") {
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
        let target = contradiction.test_target.clone();
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
            let evidence_points_to_target = evidence.target.as_deref().is_some_and(|existing| {
                generated_test_local_binding_target_matches(existing, &target, workspace_root)
            }) || evidence.test_refs.iter().any(|existing| {
                generated_test_local_binding_target_matches(existing, &target, workspace_root)
            });
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

fn generated_test_local_binding_target_matches(
    existing: &str,
    target: &str,
    workspace_root: &Utf8Path,
) -> bool {
    let Some(existing) = normalize_target_path(existing, workspace_root) else {
        return false;
    };
    let Some(target) = normalize_target_path(target, workspace_root) else {
        return false;
    };
    canonical_target_key(existing.as_str()) == canonical_target_key(target.as_str())
}

pub(crate) fn generated_test_local_binding_enrichment_exact_target_identity_fixture_passes() -> bool
{
    let workspace_root = Utf8Path::new("C:/workspace/project");
    generated_test_local_binding_target_matches(
        "tests/workflow.spec.ts",
        "tests/workflow.spec.ts",
        workspace_root,
    ) && generated_test_local_binding_target_matches(
        "C:/workspace/project/tests/workflow.spec.ts",
        "tests/workflow.spec.ts",
        workspace_root,
    ) && !generated_test_local_binding_target_matches(
        "archive/tests/workflow.spec.ts",
        "tests/workflow.spec.ts",
        workspace_root,
    ) && !generated_test_local_binding_target_matches(
        "C:/workspace/other/tests/workflow.spec.ts",
        "tests/workflow.spec.ts",
        workspace_root,
    )
}

fn generated_test_local_binding_contradictions(
    cluster: &VerificationFailureCluster,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<LanguageLocalBindingContradiction> {
    let labels = if cluster.failing_labels.is_empty() {
        extract_verification_failure_labels(raw_summary)
    } else {
        cluster.failing_labels.clone()
    };
    if labels.is_empty() {
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

    let target_sources = test_targets
        .into_iter()
        .filter_map(|target| {
            read_small_test_context_source(&target, workspace_root)
                .map(|source| (target.as_str().to_string(), source))
        })
        .collect::<Vec<_>>();
    language_generated_test_local_binding_contradictions(&labels, &target_sources, raw_summary)
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
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
        let mut inferred_source_targets = verification_cluster_source_call_site_targets(
            evidence.failure_cluster.as_ref(),
            workspace_root,
        );
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
            if let Some(source_path) = source_path_for_generated_test_target(target.as_str()) {
                if let Some(source) = normalize_target_path(&source_path, workspace_root) {
                    targets.push(source);
                }
            }
        }
        if !inferred_source_targets.is_empty() {
            inferred_source_targets.append(&mut targets);
            targets = inferred_source_targets;
        }
    }
    prioritize_repair_targets(targets)
}

fn verification_cluster_source_call_site_targets(
    cluster: Option<&VerificationFailureCluster>,
    workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let Some(cluster) = cluster else {
        return Vec::new();
    };
    let targets = cluster
        .evidence
        .iter()
        .filter_map(|evidence| evidence.call_site.as_deref())
        .flat_map(language_source_targets_from_text)
        .filter_map(|target| normalize_target_path(&target, workspace_root))
        .filter(|target| is_code_or_test_target(target) && !is_test_focus_target(target))
        .collect::<Vec<_>>();
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
                        || evidence.call_site.as_deref().is_some_and(|call_site| {
                            !language_source_targets_from_text(call_site).is_empty()
                        }))
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
    _workspace_root: &Utf8Path,
) -> Vec<Utf8PathBuf> {
    let file_authoritative = targets
        .iter()
        .filter(|target| is_verification_repair_authority_target(target.as_path()))
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
        .filter(|target| is_verification_repair_authority_target(target.as_path()))
        .cloned()
        .collect::<Vec<_>>();
    let file_authoritative = failure_targets
        .iter()
        .filter(|target| is_verification_repair_authority_target(target.as_path()))
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
        }
        if labels.len() >= MAX_VERIFICATION_FAILURE_LABELS {
            break;
        }
    }
    if labels.is_empty() {
        return language_failure_labels_from_summary(summary, MAX_VERIFICATION_FAILURE_LABELS);
    }
    labels
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

    for possibility in language_verification_target_candidates(candidate) {
        if let Some(path) = normalize_target_path(&possibility, workspace_root)
            .filter(|path| workspace_root.join(path).exists())
        {
            return Some(path);
        }
    }

    None
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
    if !looks_like_docs_closeout_continuation(workspace_root, latest_user, deliverables) {
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
                        docs_route_target_alias_matches(
                            workspace_root,
                            prior.as_str(),
                            deliverable.as_str(),
                        )
                    })
                })
                && looks_like_docs_only_route_contract(&combined, &docs_requested)
        })
}

fn looks_like_docs_closeout_continuation(
    workspace_root: &Utf8Path,
    text: &str,
    deliverables: &[Utf8PathBuf],
) -> bool {
    if deliverables.is_empty()
        || !deliverables
            .iter()
            .all(|target| is_documentation_target(target.as_path()))
    {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let closeout_signal = lower.contains("typed route closeout continuation")
        || lower.contains("typed stop-hook continuation")
        || lower.contains("stop-hook continuation")
        || lower.contains("missing expected artifacts")
        || lower.contains("open obligations");
    let docs_repair_signal = lower.contains("repair docs")
        || lower.contains("docs deliverable")
        || text.contains("ドキュメント");
    closeout_signal
        && docs_repair_signal
        && docs_closeout_continuation_mentions_deliverable(workspace_root, text, deliverables)
}

fn docs_closeout_continuation_mentions_deliverable(
    workspace_root: &Utf8Path,
    text: &str,
    deliverables: &[Utf8PathBuf],
) -> bool {
    let mentioned_target_keys = implementation_handoff_remaining_target_tokens(text)
        .into_iter()
        .filter_map(|target| docs_route_target_identity(workspace_root, &target))
        .collect::<BTreeSet<_>>();
    !mentioned_target_keys.is_empty()
        && deliverables.iter().any(|target| {
            docs_route_target_identity(workspace_root, target.as_str())
                .is_some_and(|key| mentioned_target_keys.contains(&key))
        })
}

pub(crate) fn state_docs_closeout_continuation_exact_target_identity_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let deliverables = vec![Utf8PathBuf::from("docs/workflow.md")];
    let exact_text =
        "typed route closeout continuation repair docs missing expected artifacts docs/workflow.md";
    let absolute_text = "typed route closeout continuation repair docs missing expected artifacts C:/workspace/project/docs/workflow.md";
    let sibling_text = "typed route closeout continuation repair docs missing expected artifacts archive/docs/workflow.md";
    let foreign_text = "typed route closeout continuation repair docs missing expected artifacts C:/workspace/other/docs/workflow.md";
    let untargeted_text =
        "typed route closeout continuation repair docs missing expected artifacts";

    looks_like_docs_closeout_continuation(workspace_root, exact_text, &deliverables)
        && looks_like_docs_closeout_continuation(workspace_root, absolute_text, &deliverables)
        && !looks_like_docs_closeout_continuation(workspace_root, sibling_text, &deliverables)
        && !looks_like_docs_closeout_continuation(workspace_root, foreign_text, &deliverables)
        && !looks_like_docs_closeout_continuation(workspace_root, untargeted_text, &deliverables)
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
        .any(|artifact| docs_route_target_alias_matches(workspace_root, target.as_str(), &artifact))
        || extract_protected_artifact_targets(text)
            .into_iter()
            .any(|artifact| {
                docs_route_target_alias_matches(workspace_root, target.as_str(), &artifact)
            })
        || workspace_root.join(target).exists()
}

fn docs_route_target_alias_matches(workspace_root: &Utf8Path, left: &str, right: &str) -> bool {
    let Some(left) = docs_route_target_identity(workspace_root, left) else {
        return false;
    };
    let Some(right) = docs_route_target_identity(workspace_root, right) else {
        return false;
    };
    left == right
}

fn docs_route_target_identity(workspace_root: &Utf8Path, target: &str) -> Option<String> {
    normalize_target_path(target, workspace_root).map(|path| {
        path.as_str()
            .replace('\\', "/")
            .trim_start_matches("./")
            .to_ascii_lowercase()
    })
}

pub(crate) fn state_docs_route_target_alias_identity_exact_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    docs_route_target_alias_matches(workspace_root, "docs/workflow.md", "docs/workflow.md")
        && docs_route_target_alias_matches(workspace_root, "./docs/workflow.md", "docs/workflow.md")
        && docs_route_target_alias_matches(
            workspace_root,
            "C:/workspace/project/docs/workflow.md",
            "docs/workflow.md",
        )
        && !docs_route_target_alias_matches(
            workspace_root,
            "C:/workspace/sibling/docs/workflow.md",
            "docs/workflow.md",
        )
        && !docs_route_target_alias_matches(
            workspace_root,
            "foreign/docs/workflow.md",
            "docs/workflow.md",
        )
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
    for dir in ["src", "tests", "data", "examples"] {
        if fs::create_dir_all(workspace.join(dir).as_std_path()).is_err() {
            return false;
        }
    }
    let files = [
        ("Cargo.toml", "[package]\nname = \"workflow-demo\"\n"),
        (
            "src/workflow.rs",
            "pub fn workflow_state() -> &'static str { \"ready\" }\n",
        ),
        (
            "tests/workflow.contract",
            "workflow_state reports ready state\n",
        ),
        ("data/workflow-sample.json", "{}"),
        ("examples/workflow-demo.md", "workflow example"),
        (
            "task.md",
            r#"
制約:
- 既存の実装コード、設定、テストは変更しないこと。今回の成果物は文書のみとすること。
- build artifact、cache、generated output、dependency を無差別に読まないこと。

Step2: `README.md` を作成する。
Step3: `basic_design.md` を作成する。
Step4: `detail_design.md` を作成する。
source / tests / data / examples の実装実態と整合させる。
"#,
        ),
        (
            "README.md",
            "overview source tests data examples src/workflow.rs tests/workflow.contract data/workflow-sample.json examples/workflow-demo.md",
        ),
        (
            "basic_design.md",
            "architecture responsibility data flow source tests data examples src/workflow.rs tests/workflow.contract data/workflow-sample.json examples/workflow-demo.md",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
    previous.active_targets = vec![Utf8PathBuf::from("docs/old-workflow-design.md")];
    previous.completion.open_work_count = 1;
    previous.completion.blocked_reason = Some(
        "Requested deliverables still require authoring in the workspace: `docs/old-workflow-design.md`."
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
            "src/workflow.rs",
            "pub fn advance(input: &str) -> String { format!(\"workflow:{input}\") }\n",
        ),
        (
            "tests/workflow.contract",
            "workflow.advance accepts a draft input and returns a workflow status string\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "現在の実装を調査し、`docs/workflow-design.md` を日本語で作成してください。実装コードと test は変更せず、確認できた事実だけを文書化してください。最後に `verify-workflow --docs` を実行してください。".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![change_id],
                changes: vec![FileChangeEvidence {
                    change_id,
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                    summary: "Added docs/workflow-design.md".to_string(),
                }],
                summary: "Added docs/workflow-design.md".to_string(),
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
            .any(|target| target.as_str() == "docs/workflow-design.md")
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
            "src/workflow.rs",
            "pub fn advance(input: &str) -> String { format!(\"workflow:{input}\") }\n",
        ),
        (
            "test_workflow.rs",
            "#[test]\nfn workflow_advance_contract() {\n    assert_eq!(workflow::advance(\"draft\"), \"workflow:draft\");\n}\n",
        ),
        (
            "docs/workflow-design.md",
            "# ワークフロー設計\n\n## 概要\n\n実装 `src/workflow.rs` と `test_workflow.rs` を確認した事実を記録します。\n\n## テスト\n\nroot-level `test_workflow.rs` は `workflow.advance` の公開契約を検証します。\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "現在の実装を調査し、`docs/workflow-design.md` を日本語で作成してください。実装コードと test は変更せず、確認できた事実だけを文書化してください。最後に `verify-workflow --docs` を実行してください。".to_string(),
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
                .any(|path| path.as_str() == "test_workflow.rs")
    });
    let deliverable_tests_satisfied = docs.deliverables.iter().any(|deliverable| {
        deliverable.target.as_str() == "docs/workflow-design.md"
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
            .any(|command| command.contains("verify-workflow --docs"))
}

pub(crate) fn state_docs_route_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    docs_route_contract_promotes_docs_repair_fixture_passes()
        && docs_route_single_deliverable_contract_promotes_docs_repair_fixture_passes()
        && docs_route_flat_test_artifact_satisfies_required_area_fixture_passes()
        && docs_route_localized_topic_completion_fixture_passes()
}

pub(crate) fn docs_route_localized_topic_completion_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    for dir in ["src", "tests", "data", "examples"] {
        if fs::create_dir_all(workspace.join(dir).as_std_path()).is_err() {
            return false;
        }
    }
    let files = [
        (
            "src/workflow.rs",
            "pub fn workflow_state() -> &'static str { \"ready\" }\n",
        ),
        (
            "tests/workflow.contract",
            "workflow_state reports ready state\n",
        ),
        ("data/workflow-sample.json", "{}"),
        ("examples/workflow-demo.md", "workflow example"),
        (
            "task.md",
            r#"
制約:
- 既存の実装コード、設定、テストは変更しないこと。今回の成果物は文書のみとすること。

Step2: `README.md` を作成する。
Step3: `basic_design.md` を作成する。
Step4: `detail_design.md` を作成する。
source / tests / data / examples の実装実態と整合させる。
"#,
        ),
        (
            "README.md",
            "概要 source tests data examples src/workflow.rs tests/workflow.contract data/workflow-sample.json examples/workflow-demo.md",
        ),
        (
            "basic_design.md",
            "アーキテクチャ 責務 データフロー source tests data examples src/workflow.rs tests/workflow.contract data/workflow-sample.json examples/workflow-demo.md",
        ),
        (
            "detail_design.md",
            "入出力\n## データモデル\nフロー source tests data examples src/workflow.rs tests/workflow.contract data/workflow-sample.json examples/workflow-demo.md",
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
        }
    }
    targets
}

fn content_changing_progress_since_latest_verification_failure(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
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
        matches!(
            &item.payload,
            HistoryItemPayload::FileChange { changes, .. }
                if file_changes_have_authoring_content_change(changes, workspace_root)
        )
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

fn is_authoring_content_change_path(path: &Utf8Path) -> bool {
    !path_is_verification_runner_byproduct_or_dependency(path)
}

fn path_is_verification_runner_byproduct_or_dependency(path: &Utf8Path) -> bool {
    language_verification_runner_byproduct_or_dependency(path.as_str())
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                text: "Create source artifact `src/workflow.rs` and then run `verify-contract --behavior`."
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
    state.process_phase == ProcessPhase::Author
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && !state.completion.closeout_ready
        && !state.completion.verification_pending
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                verification_commands,
            }) if pending_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
                && verification_commands == vec!["verify-contract --behavior".to_string()]
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create source artifact `src/workflow.rs` and test artifact `tests/workflow.behavior.md`, then run `verify-contract --behavior`."
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                    summary: "typed authoring completion evidence: Added source artifact src/workflow.rs".to_string(),
                }],
                summary: "typed authoring completion evidence: Added source artifact src/workflow.rs"
                    .to_string(),
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
        && state.active_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
        )
}

pub(crate) fn state_requested_work_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    requested_work_missing_todo_graph_stays_authoring_authority()
        && partial_requested_work_remains_authoring_phase_fixture_passes()
        && verification_failure_labels_are_not_requested_work_targets_fixture_passes()
        && verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes()
}

pub(crate) fn verification_failure_labels_are_not_requested_work_targets_fixture_passes() -> bool {
    let text = r#"
Typed route closeout continuation.

Open obligations:
- author `workflow_contract.required_transition`
- author `workflow_contract.required_projection`

Required verification failed in the latest evidence:
- `verify-contract --behavior`
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        Utf8Path::new("C:/workspace/project"),
        Some(text),
    );
    !targets
        .iter()
        .any(|target| target.as_str().starts_with("workflow_contract."))
}

pub(crate) fn verification_failure_diagnostic_paths_are_not_requested_work_targets_fixture_passes()
-> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let text = r#"
Verification repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Repair targets:
- src/workflow.rs

Failed required verification commands:
- verify-contract --behavior

Latest verification failure evidence:
- command: verify-contract --behavior
stderr: contract verification failed
  artifact: tests/workflow.behavior.md
  diagnostic path: C:/runtime/diagnostics/tool-runner.trace
  diagnostic path: C:/runtime/library/worker.trace
  observed: workflow transition output omitted required state marker
  expected: workflow transition output includes required state marker

Expected artifacts:
- src/workflow.rs
- tests/workflow.behavior.md

After the repair edit, rerun the failed required verification command(s) with shell.
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    let no_diagnostic_targets = targets.iter().all(|target| {
        let normalized = target.as_str().replace('\\', "/").to_ascii_lowercase();
        !normalized.contains("runtime/diagnostics")
            && !normalized.contains("runtime/library")
            && !normalized.contains("tool-runner.trace")
            && !normalized.contains("worker.trace")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && state.completion.verification_pending
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("verification failed"))
        && state.active_targets.iter().all(|target| {
            let normalized = target.as_str().replace('\\', "/").to_ascii_lowercase();
            !normalized.contains("runtime/diagnostics")
                && !normalized.contains("runtime/library")
                && !normalized.contains("tool-runner.trace")
                && !normalized.contains("worker.trace")
        })
}

pub(crate) fn continuation_context_symbols_are_not_requested_work_targets_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let text = r#"
Verification repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Previous final assistant message:
All tests passed.

- `src/workflow.rs`: created a source artifact. It supports public operation (`apply_transition`) and state projection (`workflow_state`).
- `tests/workflow.behavior.md`: created behavior contract observations.

Repair targets:
- src/workflow.rs

Failed required verification commands:
- verify-contract --behavior

Expected artifacts:
- src/workflow.rs
- tests/workflow.behavior.md
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    if targets.iter().any(|target| {
        matches!(
            target.as_str(),
            "apply_transition" | "workflow_state" | "workflow.apply_transition"
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
    state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && state.active_targets.iter().all(|target| {
            !matches!(
                target.as_str(),
                "apply_transition" | "workflow_state" | "workflow.apply_transition"
            )
        })
}

pub(crate) fn route_closeout_expected_artifacts_inventory_does_not_reopen_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    let text = r#"
Typed route closeout continuation.

The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete.

Open obligations:
- repair docs `docs/workflow-design.md`

Missing expected artifacts:
- none

Expected artifacts:
- src/workflow.rs
- docs/workflow-design.md
- tests/workflow.behavior.md

Expected artifacts are route inventory evidence only. They do not create new authoring targets unless the same path is listed under Open obligations or Missing expected artifacts.
"#;
    let targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(text),
    );
    if targets != vec![Utf8PathBuf::from("docs/workflow-design.md")] {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "route closeout inventory authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace_root.to_path_buf(),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
    state.active_targets == vec![Utf8PathBuf::from("docs/workflow-design.md")]
        && !state.active_targets.iter().any(|target| {
            matches!(
                target.as_str(),
                "src/workflow.rs" | "tests/workflow.behavior.md"
            )
        })
}

pub(crate) fn docs_route_closeout_continuation_preserves_docs_authority_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::write(
            workspace.join("src").join("workflow.rs").as_std_path(),
            "pub fn apply_transition() -> &'static str {\n    \"complete\"\n}\n",
        )
        .is_err()
        || fs::write(
            workspace
                .join("tests")
                .join("workflow.behavior.md")
                .as_std_path(),
            "behavior: apply_transition returns complete\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let initial_request = r#"
    現在の実装を調査し、`docs/workflow-design.md` を日本語で作成してください。
実装コードと test は変更せず、確認できた事実だけを文書化してください。
最後に `verify-contract --behavior` を実行してください。

Scenario contract authority:
- `scenario_contract.md`
- `scenario_contract.json`
"#;
    let continuation = r#"
Typed route closeout continuation.

The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete.

Open obligations:
- repair docs `docs/workflow-design.md`
- repair docs deliverable `docs/workflow-design.md`

Missing expected artifacts:
- docs/workflow-design.md

Expected artifacts:
- src/workflow.rs
- docs/workflow-design.md
- tests/workflow.behavior.md

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
        && state.active_targets == vec![Utf8PathBuf::from("docs/workflow-design.md")]
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
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace.join("src/workflow.rs").as_std_path(),
            "workflow implementation artifact\n",
        )
        .is_err()
        || fs::write(
            workspace.join("tests/workflow.behavior.md").as_std_path(),
            "workflow behavior verification artifact\n",
        )
        .is_err()
        || fs::write(
            workspace.join("docs/workflow-contract.md").as_std_path(),
            "workflow contract artifact\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Typed verification-repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Previous final assistant message:
All tests passed.

Repair targets:
- src/workflow.rs

Failed required verification commands:
- verify-contract --behavior

Latest verification failure evidence:
- command: verify-contract --behavior
- typed verification continuation evidence: source behavior contract mismatch in src/workflow.rs.

Expected artifacts:
- src/workflow.rs
- tests/workflow.behavior.md
- docs/workflow-contract.md

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
        && initial_state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
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
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            initial_active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        );

    let mut repaired_items = initial_items;
    repaired_items.push(HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::FileChange {
            call_id: crate::session::ToolCallId::new(),
            change_ids: vec![ChangeId::new()],
            changes: vec![FileChangeEvidence {
                change_id: ChangeId::new(),
                kind: crate::session::ChangeKind::Update,
                path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                summary: "Updated source implementation artifact".to_string(),
            }],
            summary: "Updated source implementation artifact".to_string(),
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

pub(crate) fn verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("build-artifacts/cache").as_std_path()).is_err()
        || fs::write(
            workspace.join("src/workflow.rs").as_std_path(),
            "pub fn render(value: i32) -> String { value.to_string() }\n",
        )
        .is_err()
        || fs::write(
            workspace
                .join("build-artifacts/cache/verification.snapshot")
                .as_std_path(),
            "runner cache\n",
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
        title: "verification repair continuation byproduct target authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Verification repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Repair targets:
- build-artifacts/cache/verification.snapshot
- src/workflow.rs

Failed required verification commands:
- verify-contract --behavior

Latest verification failure evidence:
- command: verify-contract --behavior
- typed verification continuation evidence: source behavior mismatch in src/workflow.rs.

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
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let byproduct = Utf8PathBuf::from("build-artifacts/cache/verification.snapshot");
    let failure_targets_exclude_byproduct = state.failure.as_ref().is_some_and(|failure| {
        failure.targets == vec![Utf8PathBuf::from("src/workflow.rs")]
            && !failure.targets.iter().any(|target| target == &byproduct)
    });
    state.process_phase == ProcessPhase::Repair
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && failure_targets_exclude_byproduct
        && !state
            .active_targets
            .iter()
            .any(|target| target == &byproduct)
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        )
}

pub(crate) fn generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generic generated-test source call-site authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test observed source behavior mismatch"
            .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-generated-test --callsite".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generic-generated-test-source-call-site".to_string(),
        failing_labels: vec!["tests/workflow.spec.ts::renders formatted value".to_string()],
        primary_failure: Some(
            "expected formatted operation output, observed raw value".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generated_test_artifact_api_misuse".to_string()),
            label: Some("renders formatted value".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("renderOperation".to_string()),
            call_site: Some("src/workflow.ts:42 renderOperation(value)".to_string()),
            exception: None,
            expected: Some("formatted operation output".to_string()),
            observed: Some("raw operation output".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: Vec::new(),
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.ts".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("src/workflow.ts")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.required_target.as_deref() == Some("src/workflow.ts")
                && snapshot.repair_owner == "source"
        })
}

pub(crate) fn generic_generated_test_line_column_call_site_targets_source_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generic generated-test line-column source call-site authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test observed source behavior mismatch"
            .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-generated-test --callsite".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generic-generated-test-line-column-source-call-site".to_string(),
        failing_labels: vec!["tests/workflow.spec.ts::renders formatted value".to_string()],
        primary_failure: Some(
            "expected formatted operation output, observed raw value".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("renders formatted value".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("renderOperation".to_string()),
            call_site: Some("at renderOperation (src/workflow.ts:42:7)".to_string()),
            exception: None,
            expected: Some("formatted operation output".to_string()),
            observed: Some("raw operation output".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: Vec::new(),
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("src/workflow.ts")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| snapshot.required_target.as_deref() == Some("src/workflow.ts"))
}

pub(crate) fn public_command_contract_continuation_projects_compact_source_repair_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
    {
        return false;
    }
    if fs::write(
        workspace.join("src/workflow.rs").as_std_path(),
        "source artifact role: public command handler\nbehavior: compact source command should consume argv tokens\n",
    )
    .is_err()
        || fs::write(
            workspace
                .join("tests/workflow.command-contract")
                .as_std_path(),
            "test artifact role: public command contract\nexpected: argv tokens produce compact stdout observation\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Typed verification-repair continuation.

Repair targets:
- src/workflow.rs

Failed required verification commands:
- verify-public-command --scenario compact-source
- verify-public-command --scenario malformed-argv

Latest verification failure evidence:
- command: verify-public-command --scenario compact-source
  requirement_id: public_command_contract
  evidence_kind: typed public command contract evidence
  expected: route-owned public argv command contract passes with the recorded exit code and stdout/stderr observation
  observed: argv invocation entered interactive stdin mode and reached EOF instead of processing command-line arguments
  failure_class: public_command_contract_failed: expected exit 0 but got Some(1); stdout lacked workflow result marker

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
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && state.completion.verification_pending
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-public-command --scenario compact-source")
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

pub(crate) fn state_public_command_continuation_summary_uses_typed_observation_markers_fixture_passes()
-> bool {
    let commands = vec!["verify-public-command --scenario compact-source".to_string()];
    let typed = r#"
    requirement_id: public_command_contract
    evidence_kind: typed public command contract evidence
    expected: route-owned public argv command contract passes
    observed: argv invocation entered interactive stdin mode and reached EOF instead of processing command-line arguments
    "#;
    let raw_exception_only = r#"
    requirement_id: public_command_contract
    evidence_kind: typed public command contract evidence
    observed: traceback from an adapter execution mentioned EOFError without a typed public-command observation marker
    "#;

    let Some(typed_summary) = compact_public_command_contract_continuation_summary(
        typed,
        &commands,
        Some("src/workflow.rs"),
    ) else {
        return false;
    };
    let Some(raw_summary) = compact_public_command_contract_continuation_summary(
        raw_exception_only,
        &commands,
        Some("src/workflow.rs"),
    ) else {
        return false;
    };

    typed_summary.contains("state_public_command_continuation_summary_typed_observation")
        || (typed_summary.contains("public_command_contract_failed")
            && typed_summary.contains("target=src/workflow.rs")
            && typed_summary.contains("observed=argv invocation entered interactive stdin mode")
            && typed_summary.contains("expected=direct argv command handling")
            && raw_summary.contains("public_command_contract_failed")
            && raw_summary.contains("expected=direct argv command handling")
            && !raw_summary.contains("observed=argv invocation entered interactive stdin mode"))
}

pub(crate) fn verification_repair_continuation_generated_test_parse_target_fixture_passes() -> bool
{
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
    {
        return false;
    }
    if fs::write(
        workspace.join("src/workflow.ts").as_std_path(),
        "export function render(value: number): string { return String(value); }\n",
    )
    .is_err()
        || fs::write(
            workspace.join("tests/workflow.spec.ts").as_std_path(),
            "test('render formats value', () => {\n  expect(render(3)).toBe('3');\n// parse defect: missing closing block\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let continuation = r#"
Typed verification-repair continuation.

The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed.

Repair targets:
- tests/workflow.spec.ts

Failed required verification commands:
- verify-generated-test --parse

Latest verification failure evidence:
- command: verify-generated-test --parse
- typed verification continuation evidence: typed generated-test parse-defect evidence in tests/workflow.spec.ts prevented executing the public behavior assertion.

Expected artifacts:
- src/workflow.ts
- tests/workflow.spec.ts

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

    let process_phase_ok = state.process_phase == ProcessPhase::Repair;
    let active_targets_ok =
        state.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    let failure_ok = state.failure.as_ref().is_some_and(|failure| {
        failure.kind == FailureKind::VerificationFailed
            && failure.targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    });
    let required_command_ok = state
        .verification
        .required_commands
        .iter()
        .any(|command| command == "verify-generated-test --parse");
    let active_ok = matches!(
        active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    );
    let diagnostic_targets_ok =
        diagnostic.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    let repair_lane_ok = diagnostic.repair_lane.as_ref().is_some_and(|lane| {
        lane.required_target.as_deref() == Some("tests/workflow.spec.ts")
            && lane
                .repair_control_snapshot
                .as_ref()
                .is_some_and(|snapshot| {
                    snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
                })
    });
    process_phase_ok
        && active_targets_ok
        && failure_ok
        && required_command_ok
        && active_ok
        && diagnostic_targets_ok
        && repair_lane_ok
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create source artifact `src/workflow.rs` and test artifact `tests/workflow.behavior.md`, then run `verify-contract --behavior`."
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added source artifact src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added test artifact tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "typed authoring completion evidence: Added source artifact src/workflow.rs; Added test artifact tests/workflow.behavior.md".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
        && crate::agent::completion_guard::generic_scaffold_completion_guard_fixture_passes()
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let malformed_patch = "*** Begin Patch\n*** Add File: src/workflow.rs\n+public operation value\n+  returns ready\n*** End Patch\n*** Begin Patch\n*** Add File: tests/workflow.behavior.md\n+behavior contract: value returns ready\n*** End Patch";
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
                    text: "Create source artifact `src/workflow.rs` and test artifact `tests/workflow.behavior.md`, then run `verify-contract --behavior`."
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added source artifact src/workflow.rs".to_string(),
                    }],
                summary: "typed authoring completion evidence: Added source artifact src/workflow.rs".to_string(),
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
                            "active_targets": ["tests/workflow.behavior.md"],
                            "typed_evidence": "typed invalid edit no-progress evidence"
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
        && state.active_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create source artifact `src/workflow.rs` and test artifact `tests/workflow.behavior.md`, then run `verify-contract --behavior`."
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added source artifact src/workflow.rs".to_string(),
                    }],
                summary: "typed authoring completion evidence: Added source artifact src/workflow.rs".to_string(),
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
                output_text: "Length 0 tests/workflow.behavior.md".to_string(),
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
                        "non_satisfying_paths": ["tests/workflow.behavior.md"]
                    },
                    "tool_feedback_envelope": {
                        "kind": "operation_progress_classification",
                        "operation_intent": "content_changing_authoring_required",
                        "operation_progress_class": "empty_artifact_no_progress",
                        "progress_effect": "no_progress",
                        "side_effects_applied": true,
                        "active_targets": ["tests/workflow.behavior.md"],
                        "typed_evidence": "typed empty artifact no-progress evidence"
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
        && state.active_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("tests/workflow.behavior.md")]
        )
}

pub(crate) fn state_residual_component_fixture_workflow_neutral_fixture_passes() -> bool {
    public_command_contract_continuation_projects_compact_source_repair_fixture_passes()
        && verification_repair_continuation_generated_test_parse_target_fixture_passes()
        && requested_work_completion_promotes_verification_fixture_passes()
        && invalid_authoring_edit_no_progress_preserves_missing_requested_target_fixture_passes()
        && empty_artifact_tool_output_does_not_satisfy_requested_work_fixture_passes()
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                text: "Create source artifact `src/workflow.rs` and test artifact `tests/workflow.behavior.md`, then run `verify-contract --behavior`.\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`\nTreat these files as prompt-visible, harness-owned contract references. Generated tests may assert only the listed requirement ids."
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
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
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
                && verification_commands == vec!["verify-contract --behavior".to_string()]
        )
}

pub(crate) fn same_document_update_uses_prior_authored_doc_not_contract_ref_fixture_passes() -> bool
{
    let workspace =
        std::env::temp_dir().join(format!("moyai_docs_route_same_doc_{}", ChangeId::new()));
    let Ok(workspace) = Utf8PathBuf::from_path_buf(workspace) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
    {
        return false;
    }
    if fs::write(
        workspace.join("docs/workflow-design.md").as_std_path(),
        "# Workflow Design\n\n## Overview\n\nExisting authored workflow design.\n",
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
        "{\"id\":\"scenario_contract.workflow.v1\"}\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `docs/workflow-design.md` from current implementation.\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`\nTreat these files as prompt-visible, harness-owned contract references."
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                    summary: "Created docs/workflow-design.md".to_string(),
                }],
                summary: "Created docs/workflow-design.md".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn,
            sequence_no: 3,
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
    let expected = vec![Utf8PathBuf::from("docs/workflow-design.md")];

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
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/workflow-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|deliverable| deliverable.target == Utf8PathBuf::from("docs/workflow-design.md"))
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
    for dir in ["docs", "src", "tests"] {
        if fs::create_dir_all(workspace.join(dir).as_std_path()).is_err() {
            return false;
        }
    }
    for (path, content) in [
        (
            "src/workflow.rs",
            "pub fn transition_label() -> &'static str {\n    \"ready\"\n}\n",
        ),
        (
            "tests/workflow.behavior.md",
            "behavior: transition_label returns ready\n",
        ),
        (
            "docs/workflow-design.md",
            "# Workflow design\n\n## Overview\n\n現在の実装 `src/workflow.rs` と behavior contract `tests/workflow.behavior.md` を確認し、workflow transition、state projection、validation boundary を文書化しています。\n",
        ),
        (
            "scenario_contract.md",
            "# Scenario Contract\n\nReference only.\n",
        ),
        (
            "scenario_contract.json",
            "{\"id\":\"scenario_contract.workflow.v1\"}\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "現在の実装を調査し、`docs/workflow-design.md` を日本語で作成してください。実装コードと behavior contract は変更せず、確認できた事実だけを文書化してください。".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                    summary: "Created docs/workflow-design.md".to_string(),
                }],
                summary: "Created docs/workflow-design.md".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: second_turn,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "いま作成した設計書を、workflow transition 拡張仕様へ更新してください。\nstate projection と validation boundary を扱える仕様にしてください。\nこの turn では文書だけを更新し、実装コードと behavior contract はまだ変更しないでください。\n\nScenario contract authority:\n- `scenario_contract.md`\n- `scenario_contract.json`".to_string(),
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
        && state.active_targets == vec![Utf8PathBuf::from("docs/workflow-design.md")]
        && state
            .completion
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| {
                reason.contains("same-document docs update requested")
                    && reason.contains("docs/workflow-design.md")
            })
        && matches!(
            active,
            Some(ActiveWorkContract::DocsRepair {
                ref deliverable,
                ref pending_deliverables,
                route_contract_satisfied: false,
                ..
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/workflow-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|item| item.target == Utf8PathBuf::from("docs/workflow-design.md")
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
    let relative_workspace = Utf8PathBuf::from("../project_sandbox/relative-workspace-fixture");
    let absolute_doc = current_dir
        .join(&relative_workspace)
        .join("docs/workflow-design.md");
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "relative workspace absolute file change".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: relative_workspace,
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `docs/workflow-design.md` from the current implementation. Do not change source or behavior contract artifacts. Then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(absolute_doc),
                    summary: "Added docs/workflow-design.md".to_string(),
                }],
                summary: "Added docs/workflow-design.md".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn verification_failure_active_work_outranks_stale_docs_route_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification authority outranks stale docs route".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.rs` and then run `verify-contract --behavior`."
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
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "contract failure: cannot find operation `execute_workflow`"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("verification-failed".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "src/workflow.rs: cannot find operation `execute_workflow`"
                        .to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "workflow-api-contract".to_string(),
                        failing_labels: vec!["verify-contract --behavior".to_string()],
                        primary_failure: Some(
                            "cannot find operation `execute_workflow`".to_string(),
                        ),
                        evidence: Vec::new(),
                        sibling_obligations: Vec::new(),
                        source_refs: vec!["src/workflow.rs".to_string()],
                        test_refs: vec!["tests/workflow.behavior.md".to_string()],
                    }),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let mut previous = SessionStateSnapshot::default();
    previous.route = TaskRoute::Docs;
    previous.process_phase = ProcessPhase::Author;
    previous.active_targets = vec![Utf8PathBuf::from("docs/stale-design.md")];
    previous.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/stale-design.md")),
        pending_deliverables: vec![DocsPendingDeliverable {
            target: Utf8PathBuf::from("docs/stale-design.md"),
            summary: "stale pending docs authority".to_string(),
        }],
        survey_packet_summary: None,
        area_coverage: Vec::new(),
        deliverables: Vec::new(),
        factual_checks: Vec::new(),
    });
    previous.completion.route_contract_pending = true;
    previous.completion.open_work_count = 1;

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);

    state.completion.verification_pending
        && !state.completion.route_contract_pending
        && state.process_phase == ProcessPhase::Repair
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/stale-design.md")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ref targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "src/workflow.rs")
        )
}

pub(crate) fn verification_failure_with_docs_reference_still_outranks_stale_docs_route_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "verification authority ignores docs reference context".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Use `docs/stale-design.md` only as background. Fix `src/workflow.rs` and run `verify-contract --behavior`.".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "contract failure: cannot find operation `execute_workflow`".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("verification-failed".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "src/workflow.rs: cannot find operation `execute_workflow`"
                        .to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "workflow-api-contract".to_string(),
                        failing_labels: vec!["verify-contract --behavior".to_string()],
                        primary_failure: Some(
                            "cannot find operation `execute_workflow`".to_string(),
                        ),
                        evidence: Vec::new(),
                        sibling_obligations: Vec::new(),
                        source_refs: vec!["src/workflow.rs".to_string()],
                        test_refs: vec!["tests/workflow.behavior.md".to_string()],
                    }),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let mut previous = SessionStateSnapshot::default();
    previous.route = TaskRoute::Docs;
    previous.process_phase = ProcessPhase::Author;
    previous.active_targets = vec![Utf8PathBuf::from("docs/stale-design.md")];
    previous.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/stale-design.md")),
        pending_deliverables: vec![DocsPendingDeliverable {
            target: Utf8PathBuf::from("docs/stale-design.md"),
            summary: "stale pending docs authority".to_string(),
        }],
        survey_packet_summary: None,
        area_coverage: Vec::new(),
        deliverables: Vec::new(),
        factual_checks: Vec::new(),
    });
    previous.completion.route_contract_pending = true;
    previous.completion.open_work_count = 1;

    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);

    state.completion.verification_pending
        && !state.completion.route_contract_pending
        && state.process_phase == ProcessPhase::Repair
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/stale-design.md")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ref targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "src/workflow.rs")
        )
}

pub(crate) fn requested_work_absolute_docs_file_change_promotes_verification_fixture_passes() -> bool
{
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let workspace = Utf8PathBuf::from("C:\\workspace\\project");
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "absolute docs verification promotion".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace.clone(),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `docs/workflow-design.md` from the current implementation. Do not change source or behavior contract artifacts. Then run `verify-contract --behavior`.".to_string(),
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
                call_id: ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from(
                        "C:/workspace/project/docs/workflow-design.md",
                    )),
                    summary: "Added docs/workflow-design.md".to_string(),
                }],
                summary: "Added docs/workflow-design.md".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
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
            title: "Run shell command: verify-contract --behavior".to_string(),
            output_text: "behavior contract passed\n".to_string(),
            metadata: Value::Null,
            success: Some(true),
            progress_effect: ToolProgressEffect::VerificationPassed,
            blocked_action: None,
            result_hash: Some("verification-pass".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "verify-contract --behavior".to_string(),
                status: VerificationRunStatus::Passed,
                exit_code: Some(0),
                timed_out: false,
                output_summary: "behavior contract passed".to_string(),
                failure_cluster: None,
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    });
    let verified_state =
        reduce_session_state_from_history_items(&session, &verified_items, &[], &state);
    let escaped_absolute_file_change_ok =
        requested_work_escaped_absolute_docs_file_change_promotes_verification_fixture_passes();
    authored_state_ok
        && verified_state.active_targets.is_empty()
        && verified_state.completion.open_work_count == 0
        && !verified_state.completion.verification_pending
        && verified_state.completion.closeout_ready
        && escaped_absolute_file_change_ok
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `docs/workflow-design.md` from the current implementation. Then run `verify-contract --behavior`.".to_string(),
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
                call_id: ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from(
                        r"C:\\workspace\\project\\docs\\workflow-design.md",
                    )),
                    summary: "Added docs/workflow-design.md".to_string(),
                }],
                summary: "Added docs/workflow-design.md".to_string(),
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

pub(crate) fn requested_work_repair_continuation_expected_artifacts_do_not_reopen_fixture_passes()
-> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::write(
            workspace.join("src/workflow.rs").as_std_path(),
            "pub fn execute_workflow() -> &'static str {\n    \"ready\"\n}\n",
        )
        .is_err()
        || fs::write(
            workspace.join("docs/workflow-design.md").as_std_path(),
            "# Workflow design\n",
        )
        .is_err()
        || fs::write(
            workspace.join("tests/workflow.behavior.md").as_std_path(),
            "behavior: execute_workflow returns ready\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
- src/workflow.rs

Failed required verification commands:
- verify-contract --behavior

Expected artifacts:
- src/workflow.rs
- docs/workflow-design.md
- tests/workflow.behavior.md

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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                    path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                    summary: "Updated src/workflow.rs".to_string(),
                }],
                summary: "Updated src/workflow.rs".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );

    let normal_docs_targets = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace.as_path(),
        Some("Create `docs/workflow-design.md`, then run `verify-contract --behavior`."),
    );

    continuation_ok && normal_docs_targets == vec![Utf8PathBuf::from("docs/workflow-design.md")]
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                call_id: crate::session::ToolCallId::new(),
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

pub(crate) fn metadata_only_tool_output_does_not_satisfy_file_change_authority_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "metadata-only file change is diagnostic".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.rs`.".to_string(),
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
                title: "Write src/workflow.rs".to_string(),
                output_text: "metadata-only diagnostic".to_string(),
                metadata: json!({
                    "operation_progress_class": "content_changing_progress",
                    "changed_files": ["src/workflow.rs"],
                    "changes": [{
                        "path_after": "src/workflow.rs"
                    }]
                }),
                success: Some(true),
                progress_effect: ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("metadata-only".to_string()),
                verification_run: None,
            },
        },
    ];
    let prior_state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        completion: CompletionState {
            open_work_count: 1,
            closeout_ready: false,
            verification_pending: false,
            blocked_reason: Some(
                ActiveWorkContract::RequestedWorkAuthoring {
                    pending_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
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
    state.process_phase == ProcessPhase::Author
        && !state.completion.closeout_ready
        && state.completion.open_work_count == 1
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("src/workflow.rs")]
        )
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                call_id: crate::session::ToolCallId::new(),
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                call_id: crate::session::ToolCallId::new(),
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow contract verification passed".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "workflow contract verification passed".to_string(),
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
            sequence_no: 39,
            created_at_ms: 100,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "`docs/workflow-design.md` の拡張仕様に合わせて source と behavior test を更新してください。\n\n要件:\n- `src/workflow.rs` に `execute_workflow` の分岐を追加すること。\n- 入力 validation と error handling を設計書と一致させること。\n- `tests/workflow.behavior.md` に追加仕様の behavior scenario を入れること。\n\n最後に `verify-contract --behavior` を実行して成功を確認してから終了してください。".to_string(),
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
            sequence_no: 40,
            created_at_ms: 136,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Updated src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Updated tests/workflow.behavior.md".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                        path_after: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                        summary: "Updated docs/workflow-design.md".to_string(),
                    },
                ],
                summary: "Updated src/workflow.rs, tests/workflow.behavior.md, and docs/workflow-design.md".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        )
}

pub(crate) fn state_authority_projection_uses_single_requested_work_owner_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::write(
            workspace.join("src/workflow.rs").as_std_path(),
            "pub fn value() -> i32 { 1 }\n",
        )
        .is_err()
        || fs::write(
            workspace.join("tests/workflow.behavior.md").as_std_path(),
            "#[test]\nfn value_is_one() { assert_eq!(1, 1); }\n",
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
        title: "single state authority owner".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: workspace,
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let previous = SessionStateSnapshot {
        route: TaskRoute::Docs,
        process_phase: ProcessPhase::Repair,
        active_targets: vec![Utf8PathBuf::from("docs/stale-design.md")],
        completion: CompletionState {
            route_contract_pending: true,
            blocked_reason: Some("stale docs route contract".to_string()),
            ..CompletionState::default()
        },
        docs_route: Some(DocsRouteState {
            active_deliverable: Some(Utf8PathBuf::from("docs/stale-design.md")),
            pending_deliverables: vec![DocsPendingDeliverable {
                target: Utf8PathBuf::from("docs/stale-design.md"),
                summary: "stale docs route contract".to_string(),
            }],
            ..DocsRouteState::default()
        }),
        ..SessionStateSnapshot::default()
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
                text: "Update `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`."
                    .to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let candidates = state_authority_candidates_for_history_items(&session, &items, &previous);
    let candidate_refs = candidates
        .iter()
        .map(|candidate| candidate.invariant_ref)
        .collect::<BTreeSet<_>>();
    let candidate_decision = candidates
        .into_iter()
        .min_by_key(|candidate| (candidate.precedence, candidate.invariant_ref))
        .map(StateAuthorityCandidate::into_decision);
    let state = reduce_session_state_from_history_items(&session, &items, &[], &previous);
    let active = active_work_contract_for_history_items(&session, &items, &state, &[]);
    let expected_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    candidate_refs.contains("latest_requested_work_authoring")
        && candidate_refs.contains("docs_route_active_target_authority")
        && matches!(
            candidate_decision,
            Some(StateAuthorityDecision {
                owner: StateAuthorityOwner::RequestedWorkAuthoring,
                active_work: ActiveWorkContract::RequestedWorkAuthoring { .. }
            })
        )
        && state.route == TaskRoute::Code
        && state.process_phase == ProcessPhase::Author
        && !state.completion.route_contract_pending
        && !state.completion.verification_pending
        && state.active_targets == expected_targets
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                verification_commands
            }) if pending_targets == expected_targets
                && verification_commands == vec!["verify-contract --behavior".to_string()]
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
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
    {
        return false;
    }
    for (path, content) in [
        (
            "src/workflow.rs",
            "pub fn execute_workflow(input: &str) -> &str { input }\n",
        ),
        (
            "tests/workflow.behavior.md",
            "# Workflow behavior\n\n- execute_workflow returns the accepted input.\n",
        ),
        (
            "docs/workflow-design.md",
            "# Workflow design\n\nAdd validation and error handling behavior.\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                text: "`docs/workflow-design.md` の拡張仕様に合わせて source と behavior test を更新してください。\n\n要件:\n- `src/workflow.rs` に `execute_workflow` の validation 分岐を追加すること。\n- error handling を設計書と一致させること。\n- `tests/workflow.behavior.md` に追加仕様の behavior scenario を入れること。\n\n最後に `verify-contract --behavior` を実行して成功を確認してから終了してください。".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        && authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "tests/workflow.behavior.md")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/workflow-design.md")
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets.iter().any(|target| target.as_str() == "src/workflow.rs")
                && pending_targets.iter().any(|target| target.as_str() == "tests/workflow.behavior.md")
                && !pending_targets.iter().any(|target| target.as_str() == "docs/workflow-design.md")
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Updated src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Updated tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Updated src/workflow.rs and tests/workflow.behavior.md".to_string(),
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
            .any(|target| target.as_str() == "docs/workflow-design.md")
        && completed_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-contract --behavior")
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
    if fs::create_dir_all(workspace.join("src").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("tests").as_std_path()).is_err()
        || fs::create_dir_all(workspace.join("docs").as_std_path()).is_err()
    {
        return false;
    }
    for (path, content) in [
        (
            "src/workflow.rs",
            "pub fn execute_workflow(input: &str) -> &str { input }\n",
        ),
        (
            "tests/workflow.behavior.md",
            "# Workflow behavior\n\n- execute_workflow returns the accepted input.\n",
        ),
        (
            "docs/workflow-design.md",
            "# Workflow design\n\nCurrent workflow operation behavior.\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                text: "前回作成した `docs/workflow-design.md` を更新してください。\n今回は同じ設計書だけを編集対象にし、実装成果物とテスト成果物は変更しないでください。\n\n追加仕様:\n- validation rule\n- retry policy\n- error handling 方針\n- public behavior scenario\n\n最後に `verify-contract --behavior` を実行して既存成果物が壊れていないことを確認してください。".to_string(),
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
            .any(|target| target.as_str() == "docs/workflow-design.md")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "tests/workflow.behavior.md")
        && authoring_state.route == TaskRoute::Docs
        && authoring_state.docs_route.is_some()
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::DocsRepair {
                ref deliverable,
                ref pending_deliverables,
                ..
            }) if deliverable.as_ref() == Some(&Utf8PathBuf::from("docs/workflow-design.md"))
                && pending_deliverables
                    .iter()
                    .any(|deliverable| deliverable.target == Utf8PathBuf::from("docs/workflow-design.md"))
        );
    if !docs_update_is_authoring {
        return false;
    }

    if fs::write(
        session.cwd.join("docs/workflow-design.md").as_std_path(),
        "# Workflow design\n\n## Overview\n\nUpdated docs describe repository evidence from `src/workflow.rs` and `tests/workflow.behavior.md`, and define the validation rule, retry policy, error handling 方針, and public behavior scenario for the workflow specification while leaving implementation and test artifacts unchanged.\n",
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                    path_after: Some(Utf8PathBuf::from("docs/workflow-design.md")),
                    summary: "Updated docs/workflow-design.md".to_string(),
                }],
                summary: "Updated docs/workflow-design.md".to_string(),
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
            .any(|command| command == "verify-contract --behavior")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                call_id: crate::session::ToolCallId::new(),
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
                call_id: crate::session::ToolCallId::new(),
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
            "src/workflow.rs",
            "pub fn execute_workflow(input: &str) -> String { input.trim().to_string() }\n",
        ),
        (
            "tests/workflow.behavior.md",
            "Behavior: execute_workflow trims the provided input and returns the normalized workflow output.\n",
        ),
    ] {
        if let Some(parent) = workspace.join(path).parent() {
            if fs::create_dir_all(parent.as_std_path()).is_err() {
                return false;
            }
        }
        if fs::write(workspace.join(path).as_std_path(), content).is_err() {
            return false;
        }
    }

    let text = "この同じセッションで docs/workflow-readme.md を追加し、src/workflow.rs の使い方と behavior contract の検証方法を短く書いてください。最後に verify-contract --behavior を実行して確認してください。";
    let contract = requested_work_contract_from_instruction_text(text);
    if contract.deliverable_targets != vec!["docs/workflow-readme.md".to_string()]
        || !contract
            .reference_inputs
            .iter()
            .any(|target| target == "src/workflow.rs")
        || contract
            .deliverable_targets
            .iter()
            .any(|target| target == "src/workflow.rs")
        || !contract
            .verification_commands
            .iter()
            .any(|command| command == "verify-contract --behavior")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
    let expected_initial_targets = vec![Utf8PathBuf::from("docs/workflow-readme.md")];
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("docs/workflow-readme.md")),
                    summary: "Added docs/workflow-readme.md".to_string(),
                }],
                summary: "Added docs/workflow-readme.md".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        || !authored_state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-contract --behavior")
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract passed".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("docs-output-reference-code-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "workflow behavior contract passed".to_string(),
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
            .all(|target| target.as_str() != "src/workflow.rs")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let cluster = VerificationFailureCluster {
        cluster_id: "workflow-repair-contract".to_string(),
        failing_labels: vec!["workflow_behavior_contract".to_string()],
        primary_failure: Some("workflow behavior contract mismatch".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_public_contract_mismatch".to_string()),
            label: Some("workflow_behavior_contract".to_string()),
            target: Some("tests/workflow.behavior.md".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(input)".to_string()),
            exception: None,
            expected: Some("normalized workflow output".to_string()),
            observed: Some("raw workflow output".to_string()),
            public_state_assertions: vec!["execute_workflow(input)".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["source_public_contract_mismatch".to_string()],
            sibling_obligations: vec!["execute_workflow(input)".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["execute_workflow(input)".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow behavior contract mismatch".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        && state
            .active_targets
            .iter()
            .all(|target| target.as_str() != "tests/workflow.behavior.md")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "normalized workflow output")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let output_summary = "runtime loader frame omitted; workflow export contract mismatch";
    let evidence = vec![VerificationFailureEvidence {
        evidence_kind: "verification_failure".to_string(),
        subtype: Some("runtime_loader_frame_excluded".to_string()),
        label: Some("workflow_export_contract".to_string()),
        target: Some("tests/workflow.spec.ts".to_string()),
        symbol: Some("execute_workflow".to_string()),
        call_site: Some("execute_workflow(input)".to_string()),
        exception: None,
        expected: Some("exported workflow operation".to_string()),
        observed: Some("missing workflow operation".to_string()),
        public_state_assertions: vec!["execute_workflow(input)".to_string()],
        public_missing_attributes: vec!["execute_workflow".to_string()],
        evidence_markers: vec!["runtime_loader_frame_excluded".to_string()],
        sibling_obligations: vec!["execute_workflow".to_string()],
        requirement_refs: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    }];
    let source_refs = vec!["src/workflow.rs".to_string()];
    let test_refs = vec!["tests/workflow.behavior.md".to_string()];
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-import-export-runtime-loader-frame".to_string(),
        failing_labels: vec!["workflow_export_contract".to_string()],
        primary_failure: Some("workflow export contract mismatch".to_string()),
        evidence,
        sibling_obligations: Vec::new(),
        source_refs,
        test_refs,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: output_summary.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
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
                    .any(|target| target == "src/workflow.rs")
                    && !cluster
                        .source_refs
                        .iter()
                        .any(|target| target == "runtime/loader.frame")
            })
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
        && state
            .active_targets
            .iter()
            .all(|target| target.as_str() != "tests/workflow.behavior.md")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "runtime/loader.frame")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "src/workflow.rs")
                && !targets.iter().any(|target| target.as_str() == "runtime/loader.frame")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-public-missing-near-name".to_string(),
        failing_labels: vec!["workflow_output_contract".to_string()],
        primary_failure: Some("workflow output mismatch".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("workflow_output_contract".to_string()),
            target: Some("tests/workflow.behavior.md".to_string()),
            symbol: Some("format_workflow_output".to_string()),
            call_site: Some("format_workflow_output(record)".to_string()),
            exception: None,
            expected: Some("normalized output".to_string()),
            observed: Some("missing formatter".to_string()),
            public_state_assertions: vec!["format_workflow_output(record)".to_string()],
            public_missing_attributes: vec!["format_workflow_output".to_string()],
            evidence_markers: vec![
                "`format_workflow_output` is missing; source near-name candidate is `render_workflow_output`".to_string(),
                "public missing operation `format_workflow_output`".to_string(),
            ],
            sibling_obligations: vec!["format_workflow_output".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["format_workflow_output".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
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
            title: "Run shell command: verify-contract --behavior".to_string(),
            output_text: "older verification failure".to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            result_hash: Some("old-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "verify-contract --behavior".to_string(),
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
            call_id: crate::session::ToolCallId::new(),
            change_ids: vec![ChangeId::new(), ChangeId::new()],
            changes: vec![
                FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                    path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                    summary: "Updated src/workflow.rs".to_string(),
                },
                FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                    path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                    summary: "Updated tests/workflow.behavior.md".to_string(),
                },
            ],
            summary: "Updated src/workflow.rs and tests/workflow.behavior.md".to_string(),
        },
    };
    let post_old_repair_edit = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::FileChange {
            call_id: crate::session::ToolCallId::new(),
            change_ids: vec![ChangeId::new()],
            changes: vec![FileChangeEvidence {
                change_id: ChangeId::new(),
                kind: crate::session::ChangeKind::Update,
                path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                summary: "Edited src/workflow.rs after an older failure".to_string(),
            }],
            summary: "Edited src/workflow.rs".to_string(),
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
            title: "Run shell command: verify-contract --behavior".to_string(),
            output_text:
                "workflow output formatter is missing; candidate render_workflow_output exists"
                    .to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            result_hash: Some("latest-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "verify-contract --behavior".to_string(),
                status: VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary:
                    "workflow output formatter is missing; candidate render_workflow_output exists"
                        .to_string(),
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
                    text: "Update src/workflow.rs and tests/workflow.behavior.md, then run verify-contract --behavior."
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
            .any(|target| target.as_str() == "src/workflow.rs")
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
            == Some("src/workflow.rs");
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-public-state-generated-test-only".to_string(),
        failing_labels: vec![
            "test_public_state_transition".to_string(),
            "test_public_event_projection".to_string(),
        ],
        primary_failure: Some(".....F..F............................".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("test_public_state_transition".to_string()),
            target: Some("tests/workflow.behavior.md".to_string()),
            symbol: Some("workflow.transition_status".to_string()),
            call_site: Some("workflow.transition_status(record)".to_string()),
            exception: None,
            expected: Some("completed".to_string()),
            observed: Some("pending".to_string()),
            public_state_assertions: vec![
                "workflow.transition_status(record)".to_string(),
                "record.status".to_string(),
            ],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "workflow.transition_status(record)".to_string(),
                "record.status".to_string(),
            ],
            sibling_obligations: vec![
                "workflow.transition_status(record)".to_string(),
                "record.status".to_string(),
            ],
            requirement_refs: vec!["REQ-3".to_string(), "REQ-4".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec![
            "workflow.transition_status(record)".to_string(),
            "record.status".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Updated src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Updated src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "FAIL: test_public_state_transition\nAssertionError: 'pending' != 'completed'\nFAIL: test_public_event_projection\nAssertionError: 'queued' != 'applied'".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-public-state-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: test_public_state_transition\nAssertionError: 'pending' != 'completed'\nFAIL: test_public_event_projection\nAssertionError: 'queued' != 'applied'".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: vec!["REQ-3".to_string(), "REQ-4".to_string()],
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
            .is_some_and(|target| target.as_str() == "src/workflow.rs")
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs and tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "command timed out".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::Blocked,
                blocked_action: None,
                result_hash: Some("timeout-fixture".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        && matches!(
            active_work,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "src/workflow.rs")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let label_target = "workflow_contract.required_transition diagnostic label";
    let mut stale_verify_state = SessionStateSnapshot::default();
    stale_verify_state.process_phase = ProcessPhase::Verify;
    stale_verify_state.active_targets = vec![
        Utf8PathBuf::from(label_target),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    stale_verify_state.completion.verification_pending = true;
    stale_verify_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-diagnostic-label-target-pollution".to_string(),
        failing_labels: vec!["workflow_transition_assertion_label".to_string()],
        primary_failure: Some("workflow transition assertion failed".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_transition_assertion_label".to_string()),
            target: Some(label_target.to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("workflow transition completes".to_string()),
            observed: Some("workflow transition remains pending".to_string()),
            public_state_assertions: vec!["workflow_state.status".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "workflow_state.status".to_string(),
            ],
            sibling_obligations: vec!["workflow_state.status".to_string()],
            requirement_refs: vec!["workflow_contract.required_transition".to_string()],
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["workflow_state.status".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
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
                    text: "Create src/workflow.rs and generated behavior tests.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                    path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                    summary: "Updated src/workflow.rs".to_string(),
                }],
                summary: "Updated src/workflow.rs".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                    path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                    summary: "Updated tests/workflow.behavior.md".to_string(),
                }],
                summary: "Updated tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "FAIL: workflow_transition_assertion_label\nAssertionError: pending != complete : workflow_contract.required_transition diagnostic label".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-diagnostic-label-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: workflow_transition_assertion_label\nAssertionError: pending != complete : workflow_contract.required_transition diagnostic label".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: vec!["workflow_contract.required_transition".to_string()],
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
            .is_some_and(|target| target.as_str() == "src/workflow.rs")
        && !state.active_targets.iter().any(|target| {
            target
                .as_str()
                .contains("workflow_contract.required_transition")
        })
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    verify_state.completion.verification_pending = true;
    verify_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow behavior contract mismatch".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "workflow-repair-contract".to_string(),
                        failing_labels: vec!["workflow_behavior_contract".to_string()],
                        primary_failure: Some("workflow behavior contract mismatch".to_string()),
                        evidence: vec![VerificationFailureEvidence {
                            evidence_kind: "verification_failure".to_string(),
                            subtype: Some("source_public_contract_mismatch".to_string()),
                            label: Some("workflow_behavior_contract".to_string()),
                            target: Some("tests/workflow.behavior.md".to_string()),
                            symbol: Some("execute_workflow".to_string()),
                            call_site: Some("execute_workflow(input)".to_string()),
                            exception: None,
                            expected: Some("normalized workflow output".to_string()),
                            observed: Some("raw workflow output".to_string()),
                            public_state_assertions: vec!["execute_workflow(input)".to_string()],
                            public_missing_attributes: Vec::new(),
                            evidence_markers: vec![
                                "source_public_contract_mismatch".to_string()
                            ],
                            sibling_obligations: vec!["execute_workflow(input)".to_string()],
                            requirement_refs: Vec::new(),
                            source_refs: vec!["src/workflow.rs".to_string()],
                            test_refs: vec!["tests/workflow.behavior.md".to_string()],
                        }],
                        sibling_obligations: vec!["execute_workflow(input)".to_string()],
                        source_refs: vec!["src/workflow.rs".to_string()],
                        test_refs: vec!["tests/workflow.behavior.md".to_string()],
                    }),
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
        .is_some_and(|cluster| cluster.cluster_id == "workflow-repair-contract")
        && state
            .failure
            .as_ref()
            .is_some_and(|failure| failure.summary.contains("workflow"))
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut stale_repair_state = SessionStateSnapshot::default();
    stale_repair_state.process_phase = ProcessPhase::Repair;
    stale_repair_state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    stale_repair_state.completion.verification_pending = true;
    stale_repair_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    stale_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: workflow transition contract mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: stale_repair_state.active_targets.clone(),
    });
    stale_repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "workflow-repair-contract".to_string(),
        failing_labels: vec!["workflow_transition_contract".to_string()],
        primary_failure: Some("workflow transition contract mismatch".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_contract_failure".to_string()),
            label: Some("workflow_transition_contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(\"draft\")".to_string()),
            exception: None,
            expected: Some("accepted workflow transition".to_string()),
            observed: Some("stale transition state".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["source_contract_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow behavior contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });

    let cluster = stale_repair_state
        .verification
        .failure_cluster
        .clone()
        .expect("workflow repair cluster");
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow transition contract mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow transition contract mismatch".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Updated src/workflow.rs".to_string(),
                    }],
                summary: "Updated src/workflow.rs".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        && state
            .verification
            .required_commands
            .iter()
            .any(|command| command == "verify-contract --behavior")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: false,
                ..
            })
        );

    let source_owned_session_id = crate::session::SessionId::new();
    let source_owned_turn_id = TurnId::new();
    let source_owned_session = SessionRecord {
        id: source_owned_session_id,
        project_id: crate::session::ProjectId::new(),
        title: "source-owned generated-test repair progress".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut source_owned_repair_state = SessionStateSnapshot::default();
    source_owned_repair_state.process_phase = ProcessPhase::Repair;
    source_owned_repair_state.active_targets =
        vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    source_owned_repair_state.completion.verification_pending = true;
    source_owned_repair_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    source_owned_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public workflow behavior mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: source_owned_repair_state.active_targets.clone(),
    });
    source_owned_repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-source-owned-test-only-progress".to_string(),
        failing_labels: vec!["workflow_public_behavior".to_string()],
        primary_failure: Some("public workflow behavior did not match contract".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_public_behavior".to_string()),
            target: Some("tests/workflow.behavior.md".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("public workflow behavior".to_string()),
            observed: Some("incorrect behavior transcript".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });
    let source_owned_items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_owned_session_id,
            turn_id: source_owned_turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_owned_session_id,
            turn_id: source_owned_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs and tests/workflow.behavior.md".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_owned_session_id,
            turn_id: source_owned_turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: ToolCallId::new(),
                status: ToolLifecycleStatus::Completed,
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "public workflow behavior mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("test-only-failure".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "public workflow behavior mismatch".to_string(),
                    failure_cluster: source_owned_repair_state
                        .verification
                        .failure_cluster
                        .clone(),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_owned_session_id,
            turn_id: source_owned_turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from(
                            "C:\\workspace\\project\\src\\workflow.rs",
                        )),
                        path_after: Some(Utf8PathBuf::from(
                            "C:\\workspace\\project\\src\\workflow.rs",
                        )),
                        summary: "Updated src/workflow.rs".to_string(),
                    }],
                summary: "Updated src/workflow.rs".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: source_owned_session_id,
            turn_id: source_owned_turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::SessionState {
                state: source_owned_repair_state,
            },
        },
    ];
    let source_owned_state = reduce_session_state_from_history_items(
        &source_owned_session,
        &source_owned_items,
        &[],
        &SessionStateSnapshot::default(),
    );
    let source_owned_active = active_work_contract_for_history_items(
        &source_owned_session,
        &source_owned_items,
        &source_owned_state,
        &[],
    );
    let source_owned_test_only_progress =
        matches!(source_owned_state.process_phase, ProcessPhase::Verify)
            && source_owned_state.completion.verification_pending
            && !source_owned_state
                .active_targets
                .iter()
                .any(|target| target.as_str() == "tests/workflow.behavior.md")
            && !source_owned_state
                .active_targets
                .iter()
                .any(|target| target.as_str() == "src/workflow.rs")
            && matches!(
                source_owned_active,
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut mixed_repair_state = SessionStateSnapshot::default();
    mixed_repair_state.process_phase = ProcessPhase::Repair;
    mixed_repair_state.active_targets = vec![
        Utf8PathBuf::from("tests/workflow.behavior.md"),
        Utf8PathBuf::from("src/workflow.rs"),
    ];
    mixed_repair_state.completion.verification_pending = true;
    mixed_repair_state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    mixed_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test observed public workflow mismatch"
            .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
    });
    mixed_repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-mixed-active-target-progress".to_string(),
        failing_labels: vec!["workflow_invalid_transition".to_string()],
        primary_failure: Some(
            "workflow behavior contract did not contain expected transition".to_string(),
        ),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_invalid_transition".to_string()),
            target: Some("tests/workflow.behavior.md".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("accepted workflow transition".to_string()),
            observed: Some("rejected workflow transition".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("mixed-failure".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow behavior contract mismatch".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                call_id: ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("C:/workspace/project/src/workflow.rs")),
                        path_after: Some(Utf8PathBuf::from("C:/workspace/project/src/workflow.rs")),
                        summary: "Updated src/workflow.rs".to_string(),
                    }],
                summary: "Updated src/workflow.rs".to_string(),
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

pub(crate) fn post_failure_runner_byproduct_filechange_does_not_satisfy_repair_progress_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "runner byproduct file change is not repair progress".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-source-owned-byproduct-progress".to_string(),
        failing_labels: vec!["source_contract_failure".to_string()],
        primary_failure: Some("workflow public behavior mismatch".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_contract_failure".to_string()),
            label: Some("source_contract_failure".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(\"draft\")".to_string()),
            exception: None,
            expected: Some("accepted workflow transition".to_string()),
            observed: Some("incorrect workflow state".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["source_contract_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: Vec::new(),
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: Vec::new(),
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow public behavior mismatch".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-runner-byproduct-failure".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow public behavior mismatch".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from(
                            "build-artifacts/cache/verification.snapshot",
                        )),
                        summary: "Added build-artifacts/cache/verification.snapshot".to_string(),
                    }],
                summary: "Added build-artifacts/cache/verification.snapshot".to_string(),
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
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.rs")
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets.iter().any(|target| target.as_str() == "src/workflow.rs")
        )
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let output = "Generated behavior contract overreach: expected literal presentation marker while public workflow contract only requires accepted transition.";
    let evidence = vec![VerificationFailureEvidence {
        evidence_kind: "verification_failure".to_string(),
        subtype: Some("generated_test_contract_overreach".to_string()),
        label: Some("workflow_generated_test_overreach".to_string()),
        target: Some("tests/workflow.spec.ts".to_string()),
        symbol: None,
        call_site: None,
        exception: None,
        expected: Some("presentation marker".to_string()),
        observed: Some("accepted workflow transition".to_string()),
        public_state_assertions: Vec::new(),
        public_missing_attributes: Vec::new(),
        evidence_markers: vec!["generated_test_contract_overreach".to_string()],
        sibling_obligations: Vec::new(),
        requirement_refs: Vec::new(),
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    }];
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-generated-test-public-output-overreach".to_string(),
        failing_labels: vec!["workflow_generated_test_overreach".to_string()],
        primary_failure: Some(output.to_string()),
        evidence,
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
                    text: "Create `src/workflow.ts` and `tests/workflow.spec.ts`, then run `verify-generated-test --contract`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.ts")),
                        summary: "Added src/workflow.ts".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.spec.ts")),
                        summary: "Added tests/workflow.spec.ts".to_string(),
                    },
                ],
                summary: "Added src/workflow.ts and tests/workflow.spec.ts".to_string(),
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
                title: "Run shell command: verify-generated-test --contract".to_string(),
                output_text: output.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("generated-test-output-overreach".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-generated-test --contract".to_string(),
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
            .any(|target| target.as_str() == "tests/workflow.spec.ts")
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "src/workflow.ts")
        && has_generated_test_overreach_marker
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets
                .iter()
                .any(|target| target.as_str() == "tests/workflow.spec.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs and tests/workflow.behavior.md".to_string(),
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
                title: "Run shell command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract passed".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "workflow behavior contract passed".to_string(),
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
        .push("verify-contract --behavior".to_string());

    let corrected = "verify-contract --behavior --workspace C:/workspace/project";
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
                    text: "Run `verify-contract --behavior` after authoring.".to_string(),
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
                output_text: "workflow behavior contract passed".to_string(),
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
                    output_summary: "workflow behavior contract passed".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from(
                        "build-artifacts/cache/verification.snapshot",
                    )),
                    path_after: Some(Utf8PathBuf::from(
                        "build-artifacts/cache/verification.snapshot",
                    )),
                    summary: "Updated build-artifacts/cache/verification.snapshot".to_string(),
                }],
                summary: "Updated build-artifacts/cache/verification.snapshot".to_string(),
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
                output_text: "workflow behavior contract passed".to_string(),
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
                    output_summary: "workflow behavior contract passed".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.rs`, then run `verify-contract --behavior`."
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
            turn_id: first_turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                    summary: "Added src/workflow.rs".to_string(),
                }],
                summary: "Added src/workflow.rs".to_string(),
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
                title: "Run verification command: verify-contract --behavior".to_string(),
                output_text: "workflow behavior contract passed".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "workflow behavior contract passed".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
                    text: "Create `docs/workflow-readme.md` for the workflow package.".to_string(),
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
            .any(|target| target.as_str() == "docs/workflow-readme.md")
        && matches!(
            active,
            Some(ActiveWorkContract::RequestedWorkAuthoring { .. })
        )
}

pub(crate) fn new_authoring_turn_overrides_prior_verification_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let first_turn_id = TurnId::new();
    let second_turn_id = TurnId::new();
    let temp_root = std::env::temp_dir().join(format!("moyai-new-authoring-turn-{session_id}"));
    if fs::create_dir_all(&temp_root).is_err() {
        return false;
    }
    let _cleanup = TempDirCleanup(temp_root.clone());
    if fs::create_dir_all(temp_root.join("src")).is_err()
        || fs::create_dir_all(temp_root.join("tests")).is_err()
    {
        return false;
    }
    if fs::write(
        temp_root.join("src").join("workflow.rs"),
        "pub fn execute_workflow(input: &str) -> String {\n    input.to_string()\n}\n",
    )
    .is_err()
        || fs::write(
            temp_root.join("tests").join("workflow.behavior.md"),
            "workflow behavior contract\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.process_phase = ProcessPhase::Verify;
    previous.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    previous.completion.verification_pending = true;
    previous
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs and tests/workflow.behavior.md".to_string(),
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
                    text: "Update `src/workflow.rs` and `tests/workflow.behavior.md` to support workflow replay and status projection, then run `verify-contract --behavior`.".to_string(),
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
            .any(|target| target.as_str() == "src/workflow.rs")
        && pending_targets
            .iter()
            .any(|target| target.as_str() == "tests/workflow.behavior.md")
}

pub(crate) fn state_new_authoring_turn_fixture_invariant_workspace_key_fixture_passes() -> bool {
    let invariant_workspace_key = format!("moyai-new-authoring-turn-{}", SessionId::new());
    let normalized = invariant_workspace_key.to_ascii_lowercase();
    !normalized.contains("fr10")
        && !normalized.contains("fr22")
        && !normalized.contains("case")
        && new_authoring_turn_overrides_prior_verification_fixture_passes()
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
        .push("verify-contract --behavior --schema".to_string());
    previous
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior --schema` and `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs and tests/workflow.behavior.md".to_string(),
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
                title: "Run verification command: verify-contract --behavior --schema".to_string(),
                output_text: "workflow-partial-contract schema passed".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: ToolProgressEffect::VerificationPassed,
                blocked_action: None,
                result_hash: Some("partial-passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior --schema".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "workflow-partial-contract schema passed".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: vec![
                        "verify-contract --behavior --schema".to_string()
                    ],
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
            .any(|command| command == "verify-contract --behavior")
        && state
            .verification
            .required_commands
            .iter()
            .all(|command| command != "verify-contract --behavior --schema")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "workflow-repair-contract".to_string(),
        failing_labels: vec!["workflow_public_state_contract".to_string()],
        primary_failure: Some("workflow status projection returned pending".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_public_state_contract".to_string()),
            target: Some("workflow.status".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(sample).status".to_string()),
            exception: None,
            expected: Some("status COMPLETE".to_string()),
            observed: Some("status PENDING".to_string()),
            public_state_assertions: vec!["workflow.status".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "workflow.status expected COMPLETE".to_string(),
                "public_state_assertion_mismatch".to_string(),
            ],
            sibling_obligations: vec!["workflow.status".to_string()],
            requirement_refs: vec!["workflow-repair-contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string(), "workflow.status".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["workflow.status".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
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
                    text: "Create `src/workflow.rs` and `tests/workflow.behavior.md`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.rs")),
                        summary: "Added src/workflow.rs".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.behavior.md")),
                        summary: "Added tests/workflow.behavior.md".to_string(),
                    },
                ],
                summary: "Added src/workflow.rs; Added tests/workflow.behavior.md".to_string(),
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
                title: "Run verification command: verify-contract --behavior".to_string(),
                output_text: "workflow status projection returned pending".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow status projection returned pending".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
            }) if targets.first().is_some_and(|target| target.as_str() == "src/workflow.rs")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: public command usage error returned exit code 0".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec![
        "workflow_public_command_incomplete_invocation".to_string(),
        "workflow_public_command_unknown_operation".to_string(),
    ];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "workflow-repair-contract".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("workflow status projection returned pending".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_public_command_incomplete_invocation".to_string()),
            target: Some("workflow.status".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(sample).status".to_string()),
            exception: None,
            expected: Some("status COMPLETE".to_string()),
            observed: Some("status PENDING".to_string()),
            public_state_assertions: vec!["workflow.status".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "workflow.status expected COMPLETE".to_string(),
                "public_state_assertion_mismatch".to_string(),
                "workflow.status".to_string(),
            ],
            sibling_obligations: vec!["workflow.status".to_string()],
            requirement_refs: vec!["workflow-repair-contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string(), "workflow.status".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["workflow.status".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });

    let active = active_work_contract_for_history_items(&session, &[], &state, &[]);
    matches!(
        active,
        Some(ActiveWorkContract::Verification {
            repair_required: true,
            targets,
            ..
        }) if targets == vec![Utf8PathBuf::from("src/workflow.rs")]
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-workflow-source-owned-requirement-test-ref-only".to_string(),
        failing_labels: vec![
            "workflow_behavior_timeout".to_string(),
            "workflow_invalid_transition".to_string(),
        ],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generic_verification_failure".to_string()),
            label: Some("workflow_behavior_timeout".to_string()),
            target: Some("src/workflow.ts".to_string()),
            symbol: None,
            call_site: None,
            exception: Some("bounded child process timeout".to_string()),
            expected: Some("bounded workflow verification returns".to_string()),
            observed: Some("workflow verification exceeded deadline".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["generic_verification_failure".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: vec![
                "workflow_contract.timeout_bound".to_string(),
                "workflow_contract.public_behavior".to_string(),
            ],
            source_refs: vec!["src/workflow.ts".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
                    text: "Create `src/workflow.ts` and `tests/workflow.spec.ts`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.ts")),
                        summary: "Added src/workflow.ts".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.spec.ts")),
                        summary: "Added tests/workflow.spec.ts".to_string(),
                    },
                ],
                summary: "Added src/workflow.ts; Added tests/workflow.spec.ts".to_string(),
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
                title: "Run verification command: verify-contract --behavior".to_string(),
                output_text: "workflow verification exceeded deadline".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("source-owned-requirement-timeout".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow verification exceeded deadline".to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
                    artifact_refs: Vec::new(),
                    requirement_refs: vec![
                        "workflow_contract.timeout_bound".to_string(),
                        "workflow_contract.public_behavior".to_string(),
                    ],
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
        && state.active_targets == vec![Utf8PathBuf::from("src/workflow.ts")]
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("src/workflow.ts")]
        )
        && diagnostic
            .repair_lane
            .as_ref()
            .is_some_and(|lane| lane.required_target.as_deref() == Some("src/workflow.ts"))
}

pub(crate) fn contract_visible_public_exception_active_work_targets_source_fixture_passes() -> bool
{
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "contract-visible public exception active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
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
    state.verification.failing_labels = vec!["workflow_invalid_public_input".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-contract-visible-public-exception-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("public input rejection contract not enforced".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_invalid_public_input".to_string()),
            target: Some("src/workflow.ts".to_string()),
            symbol: Some("execute_workflow".to_string()),
            call_site: Some("execute_workflow(invalid_sample)".to_string()),
            exception: Some("missing public rejection".to_string()),
            expected: Some("invalid input rejected".to_string()),
            observed: Some("invalid input accepted".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["public_exception_mismatch".to_string()],
            sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
            requirement_refs: vec!["workflow-source-contract".to_string()],
            source_refs: vec!["src/workflow.ts".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["source_public_behavior_assertion".to_string()],
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("src/workflow.ts")]
    ) && repair_lane.as_ref().is_some_and(|lane| {
        lane.required_target.as_deref() == Some("src/workflow.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: generated test has unresolved helper and workflow output mismatch"
                .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec![
        "workflow_generated_helper_executes".to_string(),
        "workflow_public_output".to_string(),
    ];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-validity-source-sibling".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_public_output".to_string()),
            target: Some("public-output".to_string()),
            symbol: None,
            call_site: Some("render_workflow_public_value()".to_string()),
            exception: Some("missing generated-test helper binding".to_string()),
            expected: Some("visible output contract".to_string()),
            observed: Some("generated helper binding unresolved".to_string()),
            public_state_assertions: vec!["render_workflow_public_value()".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated test missing helper binding".to_string(),
                "generated test name-resolution frame `tests/workflow.spec.ts`".to_string(),
                "generated_test_artifact_name_resolution_defect".to_string(),
                "public_state_assertion_mismatch".to_string(),
            ],
            sibling_obligations: vec![
                "render_workflow_public_value()".to_string(),
                "generated test executable validity".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: vec!["public-output".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec![
            "render_workflow_public_value()".to_string(),
            "generated test executable validity".to_string(),
        ],
        source_refs: vec!["public-output".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        })
}

pub(crate) fn generated_test_api_misuse_active_work_targets_test_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "generated-test api misuse active work authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test used invalid reflection subject".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_generated_reflection_subject".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-api-misuse".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generated_test_artifact_api_misuse".to_string()),
            label: Some("workflow_generated_reflection_subject".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("source_reflection_subject".to_string()),
            call_site: Some("source = inspect_source(module_name_string)".to_string()),
            exception: Some("reflection subject was a string".to_string()),
            expected: None,
            observed: Some(
                "generated test invalid reflection subject `module_name_string`".to_string(),
            ),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_artifact_api_misuse".to_string(),
                "generated test invalid reflection subject `module_name_string`".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow_contract.generated_test_api_surface".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test used invalid environment provider"
            .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_env_contract".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-module-attribute-api-misuse-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("generated_test_artifact_api_misuse".to_string()),
            label: Some("workflow_env_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: Some("environment_provider".to_string()),
            call_site: Some("env = environment_provider.snapshot()".to_string()),
            exception: Some("invalid generated-test environment provider".to_string()),
            expected: None,
            observed: Some("generated test used invalid environment provider".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_artifact_api_misuse".to_string(),
                "generated test invalid environment provider".to_string(),
                "generated test invalid module attribute `workflow_env`".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
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
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test over-specified workflow error kind"
            .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_generated_exception_overreach".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-exception-type-overreach".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("generated test over-specified workflow error kind".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_exception_mismatch".to_string()),
            label: Some("workflow_error_kind".to_string()),
            target: Some("src/workflow.ts".to_string()),
            symbol: None,
            call_site: Some("execute_workflow(invalid_sample)".to_string()),
            exception: Some("generated test over-specified error kind".to_string()),
            expected: Some("workflow contract error".to_string()),
            observed: Some("specific generated-test error kind".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_exception_mismatch".to_string(),
                "generated_test_contract_overreach".to_string(),
                "generated-test exception type assertion overreach".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.ts".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    ) && repair_lane
        .as_ref()
        .and_then(|lane| lane.repair_control_snapshot.as_ref())
        .is_some_and(|snapshot| {
            snapshot.repair_owner == "generated_test"
                && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
        })
}

pub(crate) fn state_generated_test_exception_overreach_fixture_domain_neutral_fixture_passes()
-> bool {
    let neutral_label = "workflow_generated_exception_overreach";
    !neutral_label.contains("divide_by_zero")
        && !neutral_label.contains("calculator")
        && !neutral_label.contains("calculate")
        && !neutral_label.contains("arithmetic")
        && generated_test_exception_type_overreach_active_work_targets_test_fixture_passes()
}

pub(crate) fn mixed_source_public_api_and_generated_test_name_resolution_active_work_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "mixed source/test repair authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary:
            "verification failed: workflow public operation missing and generated test helper unresolved"
                .to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-mixed-source-public-api-generated-test-name-resolution".to_string(),
        failing_labels: vec![
            "workflow_public_api".to_string(),
            "workflow_generated_helper".to_string(),
        ],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("public_class_attribute_mismatch".to_string()),
                label: Some("workflow_public_api".to_string()),
                target: None,
                symbol: Some("run_workflow".to_string()),
                call_site: Some("run_workflow(sample)".to_string()),
                exception: Some("public operation missing".to_string()),
                expected: Some("workflow result".to_string()),
                observed: Some("run_workflow is missing".to_string()),
                public_state_assertions: vec!["run_workflow(sample)".to_string()],
                public_missing_attributes: vec!["run_workflow".to_string()],
                evidence_markers: vec![
                    "public_class_attribute_mismatch".to_string(),
                    "public missing operation `run_workflow`".to_string(),
                ],
                sibling_obligations: vec!["`run_workflow` is missing".to_string()],
                requirement_refs: Vec::new(),
                source_refs: vec!["src/workflow.ts".to_string()],
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
            VerificationFailureEvidence {
                evidence_kind: "verification_failure".to_string(),
                subtype: Some("generic_verification_failure".to_string()),
                label: Some("workflow_generated_helper".to_string()),
                target: Some("tests/workflow.spec.ts".to_string()),
                symbol: Some("workflow_env".to_string()),
                call_site: Some("env = make_workflow_env(workflow_env)".to_string()),
                exception: Some("generated-test helper unresolved".to_string()),
                expected: None,
                observed: Some("missing generated-test helper name `workflow_env`".to_string()),
                public_state_assertions: Vec::new(),
                public_missing_attributes: Vec::new(),
                evidence_markers: vec![
                    "generated test helper unresolved name".to_string(),
                    "generated_test_artifact_name_resolution_defect".to_string(),
                ],
                sibling_obligations: Vec::new(),
                requirement_refs: Vec::new(),
                source_refs: Vec::new(),
                test_refs: vec!["tests/workflow.spec.ts".to_string()],
            },
        ],
        sibling_obligations: vec!["`run_workflow` is missing".to_string()],
        source_refs: vec!["src/workflow.ts".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("src/workflow.ts")]
    ) && repair_lane.as_ref().is_some_and(|lane| {
        lane.required_target.as_deref() == Some("src/workflow.ts")
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
                        && snapshot.required_target.as_deref() == Some("src/workflow.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.ts"),
        Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: generated test parse defect".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.verification.failing_labels = vec!["workflow_generated_parse".to_string()];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-generated-test-parse-defect-active-work".to_string(),
        failing_labels: state.verification.failing_labels.clone(),
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow_generated_parse".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("generated test parse defect in contract fixture".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect in generated-test artifact".to_string(),
                "source parse frame `tests/workflow.spec.ts`".to_string(),
                "source_parse_defect".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
        }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
    ) && diagnostic.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
        && diagnostic
            .active_work_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("`tests/workflow.spec.ts`"))
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner == "generated_test"
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create `src/workflow.ts` and `tests/workflow.spec.ts`, then run `verify-contract --behavior`.".to_string(),
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
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new()],
                changes: vec![FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("tests/workflow.spec.ts")),
                    summary: "Added tests/workflow.spec.ts".to_string(),
                }],
                summary: "Added tests/workflow.spec.ts".to_string(),
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
                title: "Run verification command: verify-contract --behavior".to_string(),
                output_text: "No generated workflow verification cases were discovered".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("no-tests-ran".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "Command: verify-contract --behavior\n\nNo generated workflow verification cases were discovered".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-no-tests-ran-recent-generated-test".to_string(),
                        failing_labels: Vec::new(),
                        primary_failure: Some("Command: verify-contract --behavior".to_string()),
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
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
        && state.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
        && state.failure.as_ref().is_some_and(|failure| {
            failure.targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
        })
        && matches!(
            active,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                targets,
                ..
            }) if targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
        )
        && repair_lane
            .as_ref()
            .and_then(|lane| lane.repair_control_snapshot.as_ref())
            .is_some_and(|snapshot| {
                snapshot.repair_owner.contains("generated_test")
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
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
    let source_dir = workspace_root.join("src");
    let test_dir = workspace_root.join("tests");
    if fs::create_dir_all(source_dir.as_std_path()).is_err()
        || fs::create_dir_all(test_dir.as_std_path()).is_err()
    {
        return false;
    }
    let source_path = source_dir.join("workflow.rs");
    let test_path = test_dir.join("workflow.spec.ts");
    if fs::write(
        source_path.as_std_path(),
        "pub fn workflow_tuple() -> (&'static str, &'static str, &'static str) {\n    (\"alpha\", \"marker\", \"omega\")\n}\n",
    )
    .is_err()
    {
        return false;
    }
    if fs::write(
        test_path.as_std_path(),
        "workflow generated contract: first binding must remain distinct from terminal binding\n",
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let cluster = VerificationFailureCluster {
        cluster_id: "fixture-local-binding-before-enrichment".to_string(),
        failing_labels: vec!["workflow_tuple_contract".to_string()],
        primary_failure: Some("Command: verify-contract --behavior".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow_tuple_contract".to_string()),
            target: None,
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("alpha".to_string()),
            observed: Some("omega".to_string()),
            public_state_assertions: vec!["workflow_first_binding".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "generated_test_local_binding_contradiction".to_string(),
            ],
            sibling_obligations: vec!["workflow_first_binding".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec!["workflow_first_binding".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    };
    let output_summary =
        "workflow tuple contract failed: terminal binding shadowed first binding\n";
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![ChangeId::new(), ChangeId::new()],
                changes: vec![
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("src/workflow.ts")),
                        summary: "Added src/workflow.ts".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("tests/workflow.spec.ts")),
                        summary: "Added tests/workflow.spec.ts".to_string(),
                    },
                ],
                summary: "Added src/workflow.ts; Added tests/workflow.spec.ts".to_string(),
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
                title: "Run verification command: verify-contract --behavior".to_string(),
                output_text: output_summary.to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: output_summary.to_string(),
                    failure_cluster: Some(cluster),
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
        && state.active_targets == vec![Utf8PathBuf::from("tests/workflow.spec.ts")]
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
                    && snapshot.required_target.as_deref() == Some("tests/workflow.spec.ts")
            })
}

pub(crate) fn public_class_attribute_cluster_fixture() -> VerificationFailureCluster {
    VerificationFailureCluster {
        cluster_id: "fixture-public-workflow-operation".to_string(),
        failing_labels: vec!["workflow_public_operation".to_string()],
        primary_failure: Some("workflow public operation missing".to_string()),
        evidence: vec![VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_class_attribute_mismatch".to_string()),
            label: Some("workflow_public_operation".to_string()),
            target: Some("src/workflow.ts".to_string()),
            symbol: Some("run_workflow".to_string()),
            call_site: Some("run_workflow(sample)".to_string()),
            exception: Some("public operation missing".to_string()),
            expected: Some("workflow result".to_string()),
            observed: Some("run_workflow is missing".to_string()),
            public_state_assertions: vec!["run_workflow(sample)".to_string()],
            public_missing_attributes: vec!["run_workflow".to_string()],
            evidence_markers: vec![
                "public_class_attribute_mismatch".to_string(),
                "public missing operation `run_workflow`".to_string(),
                "generated-test conflict evidence".to_string(),
            ],
            sibling_obligations: vec![
                "`run_workflow` is missing".to_string(),
                "run_workflow(sample)".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.ts".to_string(), "workflow result".to_string()],
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: vec![
            "`run_workflow` is missing".to_string(),
            "run_workflow(sample)".to_string(),
        ],
        source_refs: vec!["src/workflow.ts".to_string(), "workflow result".to_string()],
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
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
    path_is_generated_dependency_or_cache(&normalized) || normalized.ends_with(".pdf")
}

fn structured_document_path_is_generated_or_dependency(path: &Utf8Path) -> bool {
    let normalized = path.as_str().replace('\\', "/").to_ascii_lowercase();
    path_is_generated_dependency_or_cache(&normalized)
}

fn path_is_generated_dependency_or_cache(normalized: &str) -> bool {
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
                | "build-artifacts"
                | "generated"
                | "cache"
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
}

fn docs_route_path_is_flat_test_artifact(path: &Utf8Path) -> bool {
    if path.components().count() != 1 {
        return false;
    }
    classify_language_artifact_target(path.as_str()).role == ArtifactRole::Test
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

pub(crate) fn structured_document_summary_snapshot_from_history_items(
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
        let relative = path
            .strip_prefix(root)
            .map(|value| value.to_path_buf())
            .unwrap_or_else(|_| path.clone());
        if structured_document_path_is_generated_or_dependency(relative.as_path()) {
            continue;
        }
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
        targets.insert(relative.as_str().replace('\\', "/"));
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
    let output_target = canonical_target_key(&contract.output_target);

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
                call_id: _,
                changes,
                summary: _,
                ..
            } => {
                let output_changed = changes.iter().any(|change| {
                    change
                        .path_after
                        .as_ref()
                        .or(change.path_before.as_ref())
                        .map(|path| canonical_target_key(path.as_str()))
                        .is_some_and(|path| path == output_target)
                });
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

pub(crate) fn structured_document_output_progress_exact_target_identity_fixture_passes() -> bool {
    let unique = format!(
        "moyai-state-structured-doc-output-target-{}-{}",
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
        let user_text =
            "Summarize all pdf files into summary.md in batches of 1 file at a time.".to_string();
        let observed_batches = |file_change_path: &str, summary: &str| -> Option<Vec<usize>> {
            let session_id = SessionId::new();
            let turn_id = TurnId::new();
            let call_id = ToolCallId::new();
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
                        model_arguments: json!({"path": "a.pdf"}),
                        effective_arguments: json!({"path": "a.pdf"}),
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
                        result_hash: Some("structured-doc-output-target".to_string()),
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
                        call_id: crate::session::ToolCallId::new(),
                        change_ids: Vec::new(),
                        changes: vec![FileChangeEvidence {
                            change_id: ChangeId::new(),
                            path_before: None,
                            path_after: Some(Utf8PathBuf::from(file_change_path)),
                            kind: crate::session::ChangeKind::Add,
                            summary: summary.to_string(),
                        }],
                        summary: summary.to_string(),
                    },
                },
            ];
            structured_document_summary_snapshot_from_history_items(
                workspace_root.as_path(),
                &history_items,
                Some(&user_text),
            )
            .map(|snapshot| snapshot.observed_batch_sizes)
        };

        observed_batches("archive/summary.md", "summary.md updated") == Some(Vec::new())
            && observed_batches("summary.md", "archive/summary.md mentioned") == Some(vec![1])
    })();
    let _ = fs::remove_dir_all(workspace_root.as_std_path());
    result
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
                    model_arguments: json!({"path": "a.pdf"}),
                    effective_arguments: json!({"path": "a.pdf"}),
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
                    call_id: crate::session::ToolCallId::new(),
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

fn extract_docling_target(arguments_json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(arguments_json).ok()?;
    let path = value.get("path")?.as_str()?;
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim().trim_start_matches("./");
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn structured_document_docling_progress_exact_target_identity_fixture_passes() -> bool {
    if extract_docling_target(r#"{"path":"archive/a.pdf"}"#).as_deref() != Some("archive/a.pdf")
        || extract_docling_target(r#"{"path":"a.pdf"}"#).as_deref() != Some("a.pdf")
    {
        return false;
    }

    let unique = format!(
        "moyai-state-structured-doc-docling-target-{}-{}",
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
        if fs::create_dir_all(workspace_root.join("archive").as_std_path()).is_err()
            || fs::write(workspace_root.join("a.pdf").as_std_path(), b"root").is_err()
            || fs::write(
                workspace_root.join("archive").join("a.pdf").as_std_path(),
                b"archive",
            )
            .is_err()
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
                    arguments: json!({"path": "archive/a.pdf"}),
                    model_arguments: json!({"path": "archive/a.pdf"}),
                    effective_arguments: json!({"path": "archive/a.pdf"}),
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
                    title: "Docling converted archive/a.pdf".to_string(),
                    output_text: "converted".to_string(),
                    metadata: Value::Null,
                    success: Some(true),
                    progress_effect: ToolProgressEffect::MadeProgress,
                    blocked_action: None,
                    result_hash: Some("structured-doc-docling-target".to_string()),
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
                    call_id: crate::session::ToolCallId::new(),
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
        snapshot.processed_files == vec!["archive/a.pdf".to_string()]
            && snapshot.missing_files == vec!["a.pdf".to_string()]
            && snapshot.observed_batch_sizes == vec![1]
    })();
    let _ = fs::remove_dir_all(workspace_root.as_std_path());
    result
}

pub(crate) fn structured_document_summary_skips_generated_dependency_targets_fixture_passes() -> bool
{
    let unique = format!(
        "moyai-state-structured-doc-generated-dependency-targets-{}-{}",
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
        for directory in [
            "archive",
            "node_modules/pkg",
            "build-artifacts/cache",
            "data/runs",
            "generated",
            "src/generated",
        ] {
            if fs::create_dir_all(workspace_root.join(directory).as_std_path()).is_err() {
                return false;
            }
        }
        for (path, body) in [
            ("a.pdf", b"root".as_slice()),
            ("archive/b.pdf", b"archive".as_slice()),
            ("node_modules/pkg/c.pdf", b"dependency".as_slice()),
            ("build-artifacts/cache/d.pdf", b"cache".as_slice()),
            ("data/runs/e.pdf", b"run".as_slice()),
            ("generated/f.pdf", b"generated".as_slice()),
            ("src/generated/g.pdf", b"src-generated".as_slice()),
        ] {
            if fs::write(workspace_root.join(path).as_std_path(), body).is_err() {
                return false;
            }
        }
        let user_text =
            "Summarize all pdf files into summary.md in batches of 1 file at a time.".to_string();
        let Some(snapshot) = structured_document_summary_snapshot_from_history_items(
            workspace_root.as_path(),
            &[],
            Some(&user_text),
        ) else {
            return false;
        };
        let expected_files = snapshot
            .expected_files
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let allowed_files = BTreeSet::from(["a.pdf".to_string(), "archive/b.pdf".to_string()]);
        let excluded_markers = [
            "node_modules",
            "build-artifacts/cache",
            "data/runs",
            "generated",
            "src/generated",
        ];
        expected_files == allowed_files
            && snapshot
                .missing_files
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                == allowed_files
            && snapshot.expected_batch_sizes == vec![1, 1]
            && snapshot.processed_files.is_empty()
            && snapshot
                .expected_files
                .iter()
                .chain(snapshot.missing_files.iter())
                .all(|file| !excluded_markers.iter().any(|marker| file.contains(marker)))
    })();
    let _ = fs::remove_dir_all(workspace_root.as_std_path());
    result
}

fn extract_failure_paths_from_text(summary: &str, workspace_root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let targets = language_failure_paths_from_summary(summary)
        .into_iter()
        .filter_map(|value| normalize_target_path(&value, workspace_root))
        .collect::<Vec<_>>();
    prioritize_repair_targets(targets)
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

fn is_verification_repair_authority_target(path: &Utf8Path) -> bool {
    language_verification_repair_authority_target(path.as_str())
}

fn is_code_or_test_target(path: &Utf8Path) -> bool {
    matches!(
        classify_language_artifact_target(path.as_str()).role,
        ArtifactRole::Source | ArtifactRole::Test
    )
}

fn is_test_focus_target(path: &Utf8Path) -> bool {
    classify_language_artifact_target(path.as_str()).role == ArtifactRole::Test
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
        model: STATE_FIXTURE_MODEL.to_string(),
        base_url: STATE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut previous = SessionStateSnapshot::default();
    previous.route = TaskRoute::Docs;
    previous.process_phase = ProcessPhase::Repair;
    previous.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];
    previous.completion.route_contract_pending = true;
    previous.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
        pending_deliverables: vec![DocsPendingDeliverable {
            target: Utf8PathBuf::from("docs/workflow-design.md"),
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
                    text: "Update `docs/workflow-design.md` only; do not edit `src/workflow.rs` or `tests/workflow.behavior.md`.".to_string(),
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
                output_text: "verification failed: workflow source artifact appeared in docs command output".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("fixture-docs-route-verification-target-authority".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "verify-contract --behavior".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "workflow source diagnostic should not become the repair target for docs/workflow-design.md".to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-docs-route-verification-target-authority".to_string(),
                        failing_labels: vec!["docs semantic check".to_string()],
                        primary_failure: Some("docs command failed".to_string()),
                        evidence: vec![VerificationFailureEvidence {
                            evidence_kind: "verification_failure".to_string(),
                            subtype: Some("generic_verification_failure".to_string()),
                            label: Some("docs semantic check".to_string()),
                            target: Some("src/workflow.rs".to_string()),
                            symbol: None,
                            call_site: None,
                            exception: None,
                            expected: None,
                            observed: None,
                            public_state_assertions: Vec::new(),
                            public_missing_attributes: Vec::new(),
                            evidence_markers: vec![
                                "generic_verification_failure".to_string(),
                                "docs_route_verification_target_fixture_language_neutral"
                                    .to_string(),
                            ],
                            sibling_obligations: Vec::new(),
                            requirement_refs: Vec::new(),
                            source_refs: vec!["src/workflow.rs".to_string()],
                            test_refs: vec!["tests/workflow.behavior.md".to_string()],
                        }],
                        sibling_obligations: Vec::new(),
                        source_refs: vec!["src/workflow.rs".to_string()],
                        test_refs: vec!["tests/workflow.behavior.md".to_string()],
                    }),
                    satisfies_command_identities: vec!["verify-contract --behavior".to_string()],
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
        && state.active_targets == vec![Utf8PathBuf::from("docs/workflow-design.md")]
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

fn enrich_verification_failure_summary_with_language_context(
    compact_summary: &str,
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> String {
    let labels = extract_verification_failure_labels(compact_summary);
    let test_sources = verification_failure_test_context_sources(raw_summary, workspace_root);
    let requirement_contexts = language_failure_requirement_contexts_from_sources(
        &labels,
        &test_sources,
        MAX_VERIFICATION_FAILURE_LABELS,
    );
    let assertion_contexts = language_failure_assertion_contexts_from_sources(
        compact_summary,
        &labels,
        &test_sources,
        MAX_VERIFICATION_FAILURE_LABELS,
    );
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

fn verification_failure_test_context_sources(
    raw_summary: &str,
    workspace_root: &Utf8Path,
) -> Vec<String> {
    extract_failure_paths_from_text(raw_summary, workspace_root)
        .into_iter()
        .filter(|path| is_test_focus_target(path))
        .filter_map(|path| read_small_test_context_source(&path, workspace_root))
        .collect::<Vec<_>>()
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
        || language_verification_failure_summary_evidence(summary)
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

    #[test]
    fn post_failure_runner_byproduct_filechange_does_not_satisfy_repair_progress() {
        assert!(
            super::post_failure_runner_byproduct_filechange_does_not_satisfy_repair_progress_fixture_passes()
        );
    }

    #[test]
    fn verification_repair_continuation_existing_byproduct_path_is_not_repair_target() {
        assert!(
            super::verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes()
        );
    }

    #[test]
    fn verification_repair_targets_from_state_uses_common_repair_authority() {
        assert!(
            super::verification_repair_targets_from_state_uses_common_repair_authority_fixture_passes()
        );
    }

    #[test]
    fn generic_generated_test_source_call_site_targets_source_without_python_suffix() {
        assert!(
            super::generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes()
        );
    }

    #[test]
    fn generic_generated_test_line_column_call_site_targets_source() {
        assert!(
            super::generic_generated_test_line_column_call_site_targets_source_fixture_passes()
        );
    }
}

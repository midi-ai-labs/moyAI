use std::collections::{BTreeSet, HashMap};
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use serde_json::Value;

use crate::agent::completion_guard::completion_workspace_blocked_reason;
use crate::agent::prompt::{
    extract_protected_artifact_targets, looks_like_structured_document_work,
    requested_work_contract_from_instruction_text, staged_task_artifact_targets_from_text,
};
use crate::agent::verification::{
    explicit_verification_commands_from_text, looks_like_verification_command,
    looks_like_verification_failure, verification_command_identity_key,
};
use crate::protocol::{
    ContentPart, FileChangeEvidence, HistoryItem, HistoryItemId, HistoryItemPayload,
    ToolLifecycleStatus, ToolProgressEffect, TurnId, VerificationRunResult, VerificationRunStatus,
};
use crate::session::{ChangeId, ProjectId, SessionId, SessionRecord, ToolCallId};
use crate::session::{
    ContractStatus, DocsArea, DocsAreaCoverage, DocsDeliverableCoverage, DocsDeliverableKind,
    DocsFactCheck, DocsFactCheckKind, DocsGroundingCoverage, DocsGroundingRequirement,
    DocsPendingDeliverable, DocsRouteState, FailureKind, FailureState, MessagePart, MessageRole,
    ProcessPhase, SessionStateSnapshot, TaskRoute, TodoItem, Transcript,
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
                    "Requested deliverables are still missing from the workspace: {target_summary}.{verification_summary}"
                )
            }
            Self::DocsRepair {
                deliverable,
                pending_deliverables,
                pending_summary,
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
    let Some(typed_evidence) = latest_typed_verification_failure_context(session, history_items)
    else {
        return state;
    };

    let repair_progress_observed = typed_evidence.failure.targets.iter().any(|target| {
        observed_target_set_contains_path(
            &post_failure_written_targets,
            target,
            session.cwd.as_path(),
        )
    });
    state.process_phase = if repair_progress_observed {
        ProcessPhase::Verify
    } else {
        ProcessPhase::Repair
    };
    retain_targets_without_observed_progress(
        &mut state.active_targets,
        &post_failure_written_targets,
        session.cwd.as_path(),
    );
    let remaining_failure_targets = typed_evidence
        .failure
        .targets
        .clone()
        .into_iter()
        .filter(|target| {
            !observed_target_set_contains_path(
                &post_failure_written_targets,
                target,
                session.cwd.as_path(),
            )
        })
        .collect::<Vec<_>>();
    state.active_targets =
        verification_failure_repair_targets(state.active_targets, remaining_failure_targets);
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
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason = Some(format!(
        "verification failed: {}",
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
    if requested_work.pending_targets.is_empty() {
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
    let docs_route = build_docs_route_state(session.cwd.as_path(), &contract);
    let pending_deliverables = docs_route_pending_deliverables_from_parts(
        &docs_route.area_coverage,
        &docs_route.deliverables,
        &docs_route.factual_checks,
        docs_route.active_deliverable.as_ref(),
    );
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
    state.active_targets = requested_work.required_targets.clone();
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
        .filter_map(|command| verification_command_identity_key(command))
        .collect::<BTreeSet<_>>();
    for command in additional {
        let key = verification_command_identity_key(command)
            .unwrap_or_else(|| command.to_ascii_lowercase());
        if seen.insert(key) {
            existing.push(command.clone());
        }
    }
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
        latest_content_change_sequence_since_latest_user(history_items)
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
            matches!(run.status, VerificationRunStatus::Passed)
                && explicit_required_commands.iter().any(|required| {
                    verification_command_identity_key(required)
                        == verification_command_identity_key(&run.command)
                })
        })
}

fn latest_content_change_sequence_since_latest_user(history_items: &[HistoryItem]) -> Option<i64> {
    history_items_since_latest_user_turn(history_items)
        .into_iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } if !changes.is_empty() => {
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
        latest_content_change_sequence_since_latest_user(history_items);

    for item in history_items_since_latest_user_turn(history_items) {
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref()) {
                        if let Some(normalized) =
                            normalize_target_path(path.as_str(), workspace_root)
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
                    if let Some(run) = verification_run
                        && let Some(key) = verification_command_identity_key(&run.command)
                    {
                        passed_verification_command_keys.insert(key);
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
                verification_command_identity_key(command)
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
        });
    }

    if state.completion.verification_pending {
        return Some(ActiveWorkContract::Verification {
            commands: state.verification.required_commands.clone(),
            failing_labels: state.verification.failing_labels.clone(),
            repair_required: matches!(
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

    if !requested_work.pending_targets.is_empty() {
        return Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: requested_work.pending_targets,
            verification_commands: requested_work.verification_commands,
        });
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
            targets: verification_repair_targets_from_state(state)
                .unwrap_or_else(|| state.active_targets.clone()),
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
            targets: requested_work.required_targets,
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
        let normalized = target.replace('\\', "/");
        if normalized.trim().is_empty() {
            continue;
        }
        if seen.insert(normalized.to_ascii_lowercase()) {
            targets.push(Utf8PathBuf::from(normalized));
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
    Some(targets)
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
fn tool_result_part_is_nonprogress(value: &crate::session::ToolResultPart) -> bool {
    if value.success == Some(false) {
        return true;
    }
    if matches!(
        value.progress_effect,
        crate::protocol::ToolProgressEffect::NoProgress
            | crate::protocol::ToolProgressEffect::Blocked
            | crate::protocol::ToolProgressEffect::VerificationFailed
    ) {
        return true;
    }
    false
}

pub(crate) fn latest_verification_failure_context(transcript: &Transcript) -> Option<FailureState> {
    let tool_calls = tool_calls_by_call_id(transcript);
    let mut recent_change_targets = Vec::<Utf8PathBuf>::new();
    let mut latest_failure = None;
    let protected_targets = latest_user_text(transcript)
        .as_deref()
        .map(protected_artifact_targets_from_text_as_paths)
        .unwrap_or_default();

    for message in &transcript.messages {
        for part in &message.parts {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.status != crate::session::ToolCallStatus::Completed {
                continue;
            }
            let Some(meta) = tool_calls.get(&value.tool_call_id) else {
                continue;
            };

            if matches!(meta.tool_name, ToolName::Write | ToolName::ApplyPatch)
                && !tool_result_part_is_nonprogress(value)
            {
                recent_change_targets = merge_repair_targets(
                    recent_change_targets,
                    filter_protected_reference_targets(
                        extract_tool_targets(meta, transcript),
                        &protected_targets,
                    ),
                );
                continue;
            }

            if meta.tool_name != ToolName::Shell {
                continue;
            }

            let command = extract_json_string(&meta.arguments_json, "command");
            if looks_like_verification_command(command.as_deref(), &value.title)
                && looks_like_verification_failure(command.as_deref(), &value.title, &value.summary)
            {
                let summary_targets =
                    extract_failure_paths_from_text(&value.summary, &transcript.session.cwd);
                let command_targets =
                    extract_verification_scope_targets(command.as_deref(), &transcript.session.cwd);
                let import_export_targets = extract_import_error_module_paths_from_text(
                    &value.summary,
                    &transcript.session.cwd,
                );
                let mut targets = if !import_export_targets.is_empty() {
                    let mut focused = import_export_targets;
                    focused.extend(summary_targets.clone());
                    focused.extend(recent_change_targets.clone());
                    focused.extend(command_targets.clone());
                    prioritize_repair_targets(filter_protected_reference_targets(
                        focused,
                        &protected_targets,
                    ))
                } else if verification_failure_prefers_test_contract_targets(
                    &value.summary,
                    &summary_targets,
                    &command_targets,
                    &recent_change_targets,
                ) || verification_failure_contains_test_self_defect(
                    &value.summary,
                    &summary_targets,
                ) {
                    let mut focused = summary_targets.clone();
                    focused.extend(recent_change_targets.clone());
                    focused.extend(command_targets.clone());
                    prioritize_repair_targets(filter_protected_reference_targets(
                        focused,
                        &protected_targets,
                    ))
                } else {
                    let mut focused = recent_change_targets.clone();
                    focused.extend(summary_targets.clone());
                    focused.extend(command_targets.clone());
                    prioritize_repair_targets(filter_protected_reference_targets(
                        focused,
                        &protected_targets,
                    ))
                };
                if targets.is_empty() {
                    targets = recent_change_targets.clone();
                }
                latest_failure = Some(FailureState {
                    kind: FailureKind::VerificationFailed,
                    summary: enrich_verification_failure_summary_with_test_requirement_context(
                        &compact_verification_failure_summary(
                            command.as_deref(),
                            &value.title,
                            &value.summary,
                        ),
                        &value.summary,
                        &transcript.session.cwd,
                    ),
                    tool_name: Some(ToolName::Shell),
                    targets,
                });
                continue;
            }

            if looks_like_verification_command(command.as_deref(), &value.title)
                && !tool_result_part_is_nonprogress(value)
            {
                latest_failure = None;
            }
        }
    }

    latest_failure
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
    let latest_user_text = history_items_in_sequence(history_items)
        .into_iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::UserTurn { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|part| match part {
                        crate::protocol::ContentPart::Text { text } => Some(text.as_str()),
                        crate::protocol::ContentPart::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        });
    let protected_targets = latest_user_text
        .as_deref()
        .map(protected_artifact_targets_from_text_as_paths)
        .unwrap_or_default();
    let mut latest_failure = None;
    let mut recent_source_change_targets: Vec<Utf8PathBuf> = Vec::new();

    for item in history_items_in_sequence(history_items) {
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                let changed_targets = file_change_repair_targets(changes, &session.cwd)
                    .into_iter()
                    .filter(|target| !is_scenario_contract_ref(target.as_str()))
                    .collect::<Vec<_>>();
                let source_targets = changed_targets
                    .into_iter()
                    .filter(|target| !is_test_focus_target(target))
                    .collect::<Vec<_>>();
                if !source_targets.is_empty() {
                    recent_source_change_targets = source_targets;
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
                }
                VerificationRunStatus::Failed | VerificationRunStatus::TimedOut => {
                    let summary_targets =
                        extract_failure_paths_from_text(&run.output_summary, &session.cwd);
                    let command_targets =
                        extract_verification_scope_targets(Some(&run.command), &session.cwd);
                    let source_refs = run
                        .failure_cluster
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
                    targets = merge_recent_source_targets_for_source_owned_failure(
                        targets,
                        &recent_source_change_targets,
                        run.failure_cluster.as_ref(),
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
                        failure_cluster: run.failure_cluster.clone(),
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
) -> Vec<Utf8PathBuf> {
    if !verification_cluster_prefers_source_repair(cluster) {
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

fn verification_failure_prefers_test_contract_targets(
    summary: &str,
    summary_targets: &[Utf8PathBuf],
    command_targets: &[Utf8PathBuf],
    recent_change_targets: &[Utf8PathBuf],
) -> bool {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("assertionerror") && !lower.contains("assertion failed") {
        return false;
    }

    if recent_change_targets
        .iter()
        .any(|target| !is_test_focus_target(target))
    {
        return false;
    }

    let mut combined = Vec::new();
    combined.extend(summary_targets.iter().cloned());
    combined.extend(command_targets.iter().cloned());
    if combined.is_empty() {
        return false;
    }

    combined
        .into_iter()
        .all(|target| is_test_focus_target(&target))
}

fn verification_failure_contains_test_self_defect(
    summary: &str,
    summary_targets: &[Utf8PathBuf],
) -> bool {
    if !summary_targets
        .iter()
        .any(|target| is_test_focus_target(target))
    {
        return false;
    }
    let lower = summary.to_ascii_lowercase();
    if lower.contains("cannot import name") {
        return false;
    }
    if summary_targets
        .iter()
        .any(|target| !is_test_focus_target(target))
        && (lower.contains("failed to import test module")
            || lower.contains("syntaxerror:")
            || lower.contains("indentationerror:")
            || lower.contains("taberror:"))
    {
        return false;
    }
    let generated_test_expectation_drift =
        verification_failure_evidence_indicates_generated_test_expectation_drift(summary);
    lower.contains("nameerror:")
        || lower.contains("importerror:")
        || lower.contains("failed to import test module")
        || lower.contains("syntaxerror:")
        || lower.contains("unicodedecodeerror:")
        || generated_test_expectation_drift
}

fn verification_failure_evidence_indicates_generated_test_expectation_drift(summary: &str) -> bool {
    crate::agent::repair_lane::verification_failure_evidence_from_summary(
        FailureKind::VerificationFailed,
        summary,
    )
    .into_iter()
    .any(|evidence| {
        evidence.evidence_markers.iter().any(|marker| {
            let marker = marker.to_ascii_lowercase();
            marker.contains("generated-test data model contradicts")
                || marker.contains("generated test setup contradicts")
                || marker.contains("generated-test setup contradicts")
                || marker.contains("generated-test contract")
                || marker.contains("generated-test conflict evidence")
                || marker.contains("generated-test logging side-effect assertion")
        })
    })
}
fn latest_user_text(transcript: &Transcript) -> Option<String> {
    transcript
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.record.role, MessageRole::User))
        .and_then(|message| {
            let combined = message
                .parts
                .iter()
                .filter_map(|part| match &part.payload {
                    MessagePart::Text(value) => Some(value.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (!combined.trim().is_empty()).then_some(combined)
        })
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
        .map(Utf8PathBuf::from)
        .collect::<Vec<_>>();
    let required_targets = requested_deliverable_targets_from_instruction_text_for_workspace(
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
    let deliverables = requested_deliverable_targets_from_instruction_text_for_workspace(
        workspace_root,
        Some(&latest_user),
    )
    .into_iter()
    .filter(|target| is_documentation_target(target.as_path()))
    .collect::<Vec<_>>();
    if deliverables.len() < 2 || !looks_like_docs_only_route_contract(&combined, &deliverables) {
        return None;
    }
    Some(DocsRouteContract {
        instruction_text: combined,
        deliverables,
    })
}

fn looks_like_docs_only_route_contract(text: &str, deliverables: &[Utf8PathBuf]) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_docs_signal = lower.contains("docs-only")
        || lower.contains("documentation")
        || lower.contains("document")
        || lower.contains("readme")
        || text.contains("文書のみ")
        || text.contains("設計")
        || text.contains("ドキュメント");
    let has_no_code_mutation_signal = lower.contains("do not change")
        || lower.contains("don't change")
        || lower.contains("without changing")
        || lower.contains("docs-only")
        || text.contains("変更しない")
        || text.contains("文書のみ");
    has_docs_signal
        && has_no_code_mutation_signal
        && deliverables
            .iter()
            .all(|target| is_documentation_target(target.as_path()))
}

fn build_docs_route_state(
    workspace_root: &Utf8Path,
    contract: &DocsRouteContract,
) -> DocsRouteState {
    let area_coverage = docs_area_catalog()
        .into_iter()
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
    let factual_checks = docs_route_factual_checks(workspace_root);
    let deliverables = contract
        .deliverables
        .iter()
        .map(|target| {
            docs_deliverable_coverage(
                workspace_root,
                target.clone(),
                docs_deliverable_kind(target),
                &contract.instruction_text,
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
        survey_packet_summary: Some(
            "docs-only route: use repository source/config/test/data/example evidence; generated and dependency paths are not coverage authority".to_string(),
        ),
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
    instruction_text: &str,
) -> DocsDeliverableCoverage {
    let path = workspace_root.join(target.as_str());
    let content = fs::read_to_string(path.as_std_path()).unwrap_or_default();
    let representative_paths =
        docs_representative_paths_mentioned_in_text(workspace_root, &content);
    DocsDeliverableCoverage {
        target,
        kind,
        required_areas: docs_required_areas_from_instruction_text(instruction_text),
        required_topics: docs_required_topics(kind),
        satisfied_topics: docs_satisfied_topics(kind, &content),
        representative_paths,
        grounding: docs_grounding_coverage(workspace_root),
    }
}

fn docs_required_areas_from_instruction_text(text: &str) -> Vec<DocsArea> {
    let lower = text.to_ascii_lowercase();
    docs_area_catalog()
        .into_iter()
        .filter(|area| {
            lower.contains(docs_area_label(*area)) || text.contains(docs_area_label(*area))
        })
        .collect()
}

fn docs_required_topics(kind: DocsDeliverableKind) -> Vec<String> {
    let topics: &[&str] = match kind {
        DocsDeliverableKind::Readme => &[
            "overview", "backend", "frontend", "tests", "data", "examples",
        ],
        DocsDeliverableKind::BasicDesign => &[
            "architecture",
            "responsibility",
            "data flow",
            "backend",
            "frontend",
        ],
        DocsDeliverableKind::DetailDesign => &[
            "module input output",
            "data model",
            "flow",
            "backend",
            "frontend",
        ],
        DocsDeliverableKind::Other => &["repository evidence"],
    };
    topics.iter().copied().map(str::to_string).collect()
}

fn docs_satisfied_topics(kind: DocsDeliverableKind, content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    docs_required_topics(kind)
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
        "data model" => lower.contains("data model") || original.contains("主要データ"),
        "flow" => lower.contains("flow") || original.contains("フロー"),
        "repository evidence" => {
            lower.contains(".md") || lower.contains("/") || original.contains("実装")
        }
        other => lower.contains(other),
    }
}

fn docs_grounding_coverage(workspace_root: &Utf8Path) -> Vec<DocsGroundingCoverage> {
    [
        (
            DocsGroundingRequirement::BackendMetadata,
            &[
                "backend/pyproject.toml",
                "backend/package.json",
                "backend/app",
            ][..],
        ),
        (
            DocsGroundingRequirement::BackendSource,
            &["backend/app", "backend/src", "backend/main.py"][..],
        ),
        (
            DocsGroundingRequirement::BackendRoute,
            &["backend/app/api", "backend/app/routes", "backend/routes"][..],
        ),
        (
            DocsGroundingRequirement::FrontendMetadata,
            &[
                "frontend/package.json",
                "frontend/vite.config.ts",
                "frontend/next.config.js",
            ][..],
        ),
        (
            DocsGroundingRequirement::FrontendSource,
            &["frontend/app", "frontend/src", "frontend/pages"][..],
        ),
        (DocsGroundingRequirement::Examples, &["examples"][..]),
        (
            DocsGroundingRequirement::Tests,
            &["tests", "backend/tests", "frontend/tests"][..],
        ),
        (
            DocsGroundingRequirement::Data,
            &["data", "backend/data", "frontend/data"][..],
        ),
    ]
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
    .collect()
}

fn docs_route_factual_checks(workspace_root: &Utf8Path) -> Vec<DocsFactCheck> {
    [
        ("backend", DocsFactCheckKind::PathExists, "backend"),
        ("frontend", DocsFactCheckKind::PathExists, "frontend"),
        ("examples", DocsFactCheckKind::PathExists, "examples"),
        ("data", DocsFactCheckKind::PathExists, "data"),
        ("task", DocsFactCheckKind::PathExists, "task.md"),
    ]
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
    let state = reduce_session_state_from_history_items(
        &session,
        &items,
        &[],
        &SessionStateSnapshot::default(),
    );
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

fn explicit_required_verification_commands_from_history_items(
    workspace_root: &Utf8Path,
    latest_user_text: Option<&str>,
) -> Vec<String> {
    let mut commands = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(text) = latest_user_text {
        for command in explicit_verification_commands_from_text(text) {
            let key = verification_command_identity_key(&command)
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
                let key = verification_command_identity_key(&command)
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
    let start = history_items
        .iter()
        .rposition(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut targets = BTreeSet::new();
    for item in &history_items[start..] {
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    if let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                        && let Some(normalized) =
                            normalize_target_path(path.as_str(), workspace_root)
                    {
                        targets.insert(normalized.as_str().to_ascii_lowercase());
                    }
                }
            }
            HistoryItemPayload::ToolCall {
                tool,
                effective_arguments,
                arguments,
                ..
            } if matches!(tool, ToolName::Write | ToolName::ApplyPatch) => {
                let observed = observed_paths_for_tool_value(*tool, effective_arguments)
                    .or_else(|| observed_paths_for_tool_value(*tool, arguments));
                for path in observed.unwrap_or_default() {
                    if let Some(normalized) = normalize_target_path(&path, workspace_root) {
                        targets.insert(normalized.as_str().to_ascii_lowercase());
                    }
                }
            }
            _ => {}
        }
    }
    targets
}

fn observed_written_targets_since_latest_verification_failure(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> BTreeSet<String> {
    let Some(latest_failure_sequence) = history_items
        .iter()
        .filter_map(|item| {
            let HistoryItemPayload::ToolOutput {
                status,
                verification_run: Some(run),
                ..
            } = &item.payload
            else {
                return None;
            };
            if *status != ToolLifecycleStatus::Completed
                || !matches!(
                    run.status,
                    VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
                )
            {
                return None;
            }
            Some(history_item_order_scalar(item))
        })
        .max()
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
                {
                    targets.insert(normalized.as_str().to_ascii_lowercase());
                }
            }
        }
    }
    targets
}

fn observed_paths_for_tool_value(tool: ToolName, value: &Value) -> Option<Vec<String>> {
    match tool {
        ToolName::Write => value
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()]),
        ToolName::ApplyPatch => value
            .get("patch_text")
            .and_then(Value::as_str)
            .map(extract_patch_targets),
        _ => None,
    }
}

pub(crate) fn requested_work_missing_todo_graph_stays_authoring_authority() -> bool {
    true
}

pub(crate) fn partial_requested_work_remains_authoring_phase_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "partial authoring".to_string(),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                    path_after: Some(Utf8PathBuf::from("calculator.py")),
                    summary: "Added calculator.py".to_string(),
                }],
                summary: "Added calculator.py".to_string(),
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
        && state.active_targets == vec![Utf8PathBuf::from("test_calculator.py")]
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
            }) if pending_targets == vec![Utf8PathBuf::from("test_calculator.py")]
        )
}

pub(crate) fn verification_failure_labels_are_not_requested_work_targets_fixture_passes() -> bool {
    let text = r#"
Manual ST closeout continuation.

Open obligations:
- author `test_space_invader.TestBulletClass.test_bullet_creation`
- author `test_space_invader.TestBulletClass.test_bullet_destroy`

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
            .starts_with("test_space_invader.TestBulletClass")
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py; Added test_calculator.py".to_string(),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                required_next_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 21 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
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
                    text: "`docs/calculator-design.md` の拡張仕様に合わせて実装と test を更新してください。\n\n要件:\n- `calculator.py` に `pow` と `mod` を追加すること。\n- 入力値 validation と error handling を設計書と一致させること。\n- `test_calculator.py` に追加仕様の unittest を入れること。\n\n最後に `python -m unittest` を実行して成功を確認してから終了してください。".to_string(),
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
                        path_before: Some(Utf8PathBuf::from("calculator.py")),
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Updated calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("test_calculator.py")),
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Updated test_calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("docs/calculator-design.md")),
                        path_after: Some(Utf8PathBuf::from("docs/calculator-design.md")),
                        summary: "Updated docs/calculator-design.md".to_string(),
                    },
                ],
                summary: "Updated calculator.py, test_calculator.py, and docs/calculator-design.md".to_string(),
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
        ("calculator.py", "def add(a, b):\n    return a + b\n"),
        (
            "test_calculator.py",
            "import unittest\n\nclass CalculatorTest(unittest.TestCase):\n    pass\n",
        ),
        (
            "docs/calculator-design.md",
            "# Calculator design\n\nAdd power and modulo behavior.\n",
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
                text: "`docs/calculator-design.md` の拡張仕様に合わせて実装と test を更新してください。\n\n要件:\n- `calculator.py` に `pow` と `mod` を追加すること。\n- 入力値 validation と error handling を設計書と一致させること。\n- `test_calculator.py` に追加仕様の unittest を入れること。\n\n最後に `python -m unittest` を実行して成功を確認してから終了してください。".to_string(),
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
            .any(|target| target.as_str() == "calculator.py")
        && authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_calculator.py")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "docs/calculator-design.md")
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets.iter().any(|target| target.as_str() == "calculator.py")
                && pending_targets.iter().any(|target| target.as_str() == "test_calculator.py")
                && !pending_targets.iter().any(|target| target.as_str() == "docs/calculator-design.md")
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
                        path_before: Some(Utf8PathBuf::from("calculator.py")),
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Updated calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Update,
                        path_before: Some(Utf8PathBuf::from("test_calculator.py")),
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Updated test_calculator.py".to_string(),
                    },
                ],
                summary: "Updated calculator.py and test_calculator.py".to_string(),
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
            .any(|target| target.as_str() == "docs/calculator-design.md")
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
        ("calculator.py", "def add(a, b):\n    return a + b\n"),
        (
            "test_calculator.py",
            "import unittest\n\nclass CalculatorTest(unittest.TestCase):\n    pass\n",
        ),
        (
            "docs/calculator-design.md",
            "# Calculator design\n\nCurrent four-operation calculator.\n",
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
                text: "前回作成した `docs/calculator-design.md` をもとに、電卓仕様を拡張してください。\n今回は実装コードと test は変更せず、設計書だけを更新してください。\n\n追加仕様:\n- 累乗 `pow`\n- 剰余 `mod`\n- 入力値 validation\n- CLI 利用例\n- error handling 方針\n\n最後に `python -m unittest` を実行して既存実装が壊れていないことを確認してください。".to_string(),
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
            .any(|target| target.as_str() == "docs/calculator-design.md")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "calculator.py")
        && !authoring_state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_calculator.py")
        && matches!(
            authoring_active,
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets,
                ..
            }) if pending_targets == vec![Utf8PathBuf::from("docs/calculator-design.md")]
        );
    if !docs_update_is_authoring {
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
                    path_before: Some(Utf8PathBuf::from("docs/calculator-design.md")),
                    path_after: Some(Utf8PathBuf::from("docs/calculator-design.md")),
                    summary: "Updated docs/calculator-design.md".to_string(),
                }],
                summary: "Updated docs/calculator-design.md".to_string(),
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py; Added test_calculator.py".to_string(),
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
                output_text: "AttributeError: module 'calculator' has no attribute 'calculate'"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary:
                        "AttributeError: module 'calculator' has no attribute 'calculate'"
                            .to_string(),
                    failure_cluster: Some(cluster),
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
            .any(|target| target.as_str() == "calculator.py")
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_calculator.py")
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
ERROR: test_calculator (unittest.loader._FailedTest.test_calculator)\n\
----------------------------------------------------------------------\n\
ImportError: Failed to import test module: test_calculator\n\
Traceback (most recent call last):\n\
  File \"C:\\Python313\\Lib\\unittest\\loader.py\", line 396, in _find_test_path\n\
    module = self._get_module_from_name(name)\n\
  File \"C:\\Python313\\Lib\\unittest\\loader.py\", line 339, in _get_module_from_name\n\
    __import__(name)\n\
  File \"C:\\workspace\\project\\test_calculator.py\", line 4, in <module>\n\
    from calculator import add, subtract, multiply, divide, calculate\n\
ImportError: cannot import name 'calculate' from 'calculator' (C:\\workspace\\project\\calculator.py)\n\
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
        failing_labels: vec!["test_calculator".to_string()],
        primary_failure: Some("E".to_string()),
        evidence,
        sibling_obligations: Vec::new(),
        source_refs,
        test_refs,
    };
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    verify_state.active_targets = vec![
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py; Added test_calculator.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("fixture-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: output_summary.to_string(),
                    failure_cluster: Some(cluster),
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
                    .any(|target| target == "calculator.py")
                    && !cluster
                        .source_refs
                        .iter()
                        .any(|target| target == "loader.py")
            })
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "calculator.py")
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "test_calculator.py")
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
            }) if targets.iter().any(|target| target.as_str() == "calculator.py")
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
            target: Some("test_calculator.py".to_string()),
            symbol: Some("calculator._format_result".to_string()),
            call_site: Some("calculator._format_result(1.5)".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("1.5".to_string()),
            observed: Some("calculator._format_result missing".to_string()),
            public_state_assertions: vec!["calculator._format_result(1.5)".to_string()],
            public_missing_attributes: vec!["calculator._format_result".to_string()],
            evidence_markers: vec![
                "`calculator._format_result` is missing; source near-name candidate is `calculator.format_result`".to_string(),
                "public missing method `calculator._format_result`".to_string(),
            ],
            sibling_obligations: vec!["calculator._format_result".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_calculator.py".to_string()],
        }],
        sibling_obligations: vec!["calculator._format_result".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_calculator.py".to_string()],
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
            required_next_action: None,
            result_hash: Some("old-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "python -m unittest".to_string(),
                status: VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary: "older verification failure".to_string(),
                failure_cluster: Some(cluster.clone()),
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
                    path_before: Some(Utf8PathBuf::from("calculator.py")),
                    path_after: Some(Utf8PathBuf::from("calculator.py")),
                    summary: "Updated calculator.py".to_string(),
                },
                FileChangeEvidence {
                    change_id: ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("test_calculator.py")),
                    path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                    summary: "Updated test_calculator.py".to_string(),
                },
            ],
            summary: "Updated calculator.py and test_calculator.py".to_string(),
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
                path_before: Some(Utf8PathBuf::from("calculator.py")),
                path_after: Some(Utf8PathBuf::from("calculator.py")),
                summary: "Edited calculator.py after an older failure".to_string(),
            }],
            summary: "Edited calculator.py".to_string(),
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
            output_text: "AttributeError: module 'calculator' has no attribute '_format_result'. Did you mean: 'format_result'?".to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            required_next_action: None,
            result_hash: Some("latest-failure".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "python -m unittest".to_string(),
                status: VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary: "AttributeError: module 'calculator' has no attribute '_format_result'. Did you mean: 'format_result'?".to_string(),
                failure_cluster: Some(cluster),
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
                text: "Update calculator.py and test_calculator.py, then run python -m unittest."
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
        None,
    );

    let passes = matches!(state.process_phase, ProcessPhase::Repair)
        && state.completion.verification_pending
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "calculator.py")
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
            == Some("calculator.py");
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
    verify_state.active_targets = vec![Utf8PathBuf::from("space_invader.py")];
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
            target: Some("test_space_invader.py".to_string()),
            symbol: Some("space_invader.rects_overlap".to_string()),
            call_site: Some("space_invader.rects_overlap(a, b)".to_string()),
            exception: None,
            expected: Some("truthy".to_string()),
            observed: Some("False".to_string()),
            public_state_assertions: vec![
                "space_invader.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_state_assertion_mismatch".to_string(),
                "space_invader.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            sibling_obligations: vec![
                "space_invader.rects_overlap(a, b)".to_string(),
                "len(gs.player_bullets)".to_string(),
            ],
            requirement_refs: vec!["BEH-3".to_string(), "BEH-4".to_string()],
            source_refs: Vec::new(),
            test_refs: vec!["test_space_invader.py".to_string()],
        }],
        sibling_obligations: vec![
            "space_invader.rects_overlap(a, b)".to_string(),
            "len(gs.player_bullets)".to_string(),
        ],
        source_refs: Vec::new(),
        test_refs: vec!["test_space_invader.py".to_string()],
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
                    text: "Create `space_invader.py` and `test_space_invader.py`, then run `python -m unittest`.".to_string(),
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
                        path_before: Some(Utf8PathBuf::from("space_invader.py")),
                        path_after: Some(Utf8PathBuf::from("space_invader.py")),
                        summary: "Updated space_invader.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_space_invader.py")),
                        summary: "Added test_space_invader.py".to_string(),
                    },
                ],
                summary: "Updated space_invader.py; Added test_space_invader.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("fixture-public-state-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: test_beh3_rects_overlap_edge_contact\nAssertionError: False is not true\nFAIL: test_beh4_bullet_overlaps_invader_consumes_bullet\nAssertionError: 1 != 0".to_string(),
                    failure_cluster: Some(cluster),
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
            .is_some_and(|target| target.as_str() == "space_invader.py")
        && state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "space_invader.py")
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
        Utf8PathBuf::from("test_space_invader.py"),
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
            test_refs: vec!["test_space_invader.py".to_string()],
        }],
        sibling_obligations: vec!["gs.score".to_string()],
        source_refs: vec![label_target.to_string()],
        test_refs: vec!["test_space_invader.py".to_string()],
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
                    text: "Create space_invader.py and generated tests.".to_string(),
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
                    path_before: Some(Utf8PathBuf::from("space_invader.py")),
                    path_after: Some(Utf8PathBuf::from("space_invader.py")),
                    summary: "Updated space_invader.py".to_string(),
                }],
                summary: "Updated space_invader.py".to_string(),
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
                    path_before: Some(Utf8PathBuf::from("test_space_invader.py")),
                    path_after: Some(Utf8PathBuf::from("test_space_invader.py")),
                    summary: "Updated test_space_invader.py".to_string(),
                }],
                summary: "Updated test_space_invader.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("fixture-diagnostic-label-result".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "FAIL: test_update_calls_collision_BEH4\nAssertionError: 0 != 40 : BEH-4: bullet overlap assertion message".to_string(),
                    failure_cluster: Some(cluster),
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
            .is_some_and(|target| target.as_str() == "space_invader.py")
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
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
                output_text: "AttributeError: module 'calculator' has no attribute 'calculate'"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary:
                        "AttributeError: module 'calculator' has no attribute 'calculate'"
                            .to_string(),
                    failure_cluster: Some(public_class_attribute_cluster_fixture()),
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
                required_next_action: None,
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
            .is_some_and(|failure| failure.summary.contains("calculator"))
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
    ];
    stale_repair_state.completion.verification_pending = true;
    stale_repair_state
        .verification
        .required_commands
        .push("python -m unittest".to_string());
    stale_repair_state.failure = Some(FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed: calculator.calculate returns stale values".to_string(),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py; Added test_calculator.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AssertionError: '15' != 15".to_string(),
                    failure_cluster: Some(cluster),
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
                    path_before: Some(Utf8PathBuf::from("calculator.py")),
                    path_after: Some(Utf8PathBuf::from("calculator.py")),
                    summary: "Updated calculator.py".to_string(),
                }],
                summary: "Updated calculator.py".to_string(),
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

    matches!(state.process_phase, ProcessPhase::Verify)
        && state.completion.verification_pending
        && !state
            .active_targets
            .iter()
            .any(|target| target.as_str() == "calculator.py")
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py and test_calculator.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "Ran 24 tests in 0.000s\n\nOK".to_string(),
                    failure_cluster: None,
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
                    text: "Create `calculator.py`, then run `python -m unittest`.".to_string(),
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
                    path_after: Some(Utf8PathBuf::from("calculator.py")),
                    summary: "Added calculator.py".to_string(),
                }],
                summary: "Added calculator.py".to_string(),
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
                required_next_action: None,
                result_hash: Some("prior-pass".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: "OK".to_string(),
                    failure_cluster: None,
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
                    text: "Create `README.md` for the calculator app.".to_string(),
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
        temp_root.join("calculator.py"),
        "def calculate(a, op, b):\n    return a\n",
    )
    .is_err()
        || fs::write(
            temp_root.join("test_calculator.py"),
            "import unittest\n\nclass CalculatorTest(unittest.TestCase):\n    pass\n",
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
        Utf8PathBuf::from("calculator.py"),
        Utf8PathBuf::from("test_calculator.py"),
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py and test_calculator.py".to_string(),
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
                    text: "Update `calculator.py` and `test_calculator.py` to support sqrt and pow, then run `python -m unittest`.".to_string(),
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
            .any(|target| target.as_str() == "calculator.py")
        && pending_targets
            .iter()
            .any(|target| target.as_str() == "test_calculator.py")
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
                required_next_action: None,
                result_hash: Some("partial-passed-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m py_compile app.py".to_string(),
                    status: VerificationRunStatus::Passed,
                    exit_code: Some(0),
                    timed_out: false,
                    output_summary: String::new(),
                    failure_cluster: None,
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
                    text: "Create `calculator.py` and `test_calculator.py`, then run `python -m unittest`.".to_string(),
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
                        path_after: Some(Utf8PathBuf::from("calculator.py")),
                        summary: "Added calculator.py".to_string(),
                    },
                    FileChangeEvidence {
                        change_id: ChangeId::new(),
                        kind: crate::session::ChangeKind::Add,
                        path_before: None,
                        path_after: Some(Utf8PathBuf::from("test_calculator.py")),
                        summary: "Added test_calculator.py".to_string(),
                    },
                ],
                summary: "Added calculator.py; Added test_calculator.py".to_string(),
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
                output_text: "AttributeError: calculator.calculate is missing".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("real-verification".to_string()),
                verification_run: Some(VerificationRunResult {
                    command: "python -m unittest".to_string(),
                    status: VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "AttributeError: calculator.calculate is missing".to_string(),
                    failure_cluster: Some(cluster),
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
            }) if targets.first().is_some_and(|target| target.as_str() == "calculator.py")
        )
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
            target: Some("calculator.py".to_string()),
            symbol: Some("calculator.calculate".to_string()),
            call_site: Some("calculator.calculate(\"10 + 5\")".to_string()),
            exception: Some("AttributeError".to_string()),
            expected: Some("15".to_string()),
            observed: Some("calculator.calculate is missing".to_string()),
            public_state_assertions: vec!["calculator.calculate(\"10 + 5\")".to_string()],
            public_missing_attributes: vec!["calculator.calculate".to_string()],
            evidence_markers: vec![
                "public_class_attribute_mismatch".to_string(),
                "public missing method `calculator.calculate`".to_string(),
                "generated-test conflict evidence".to_string(),
            ],
            sibling_obligations: vec![
                "`calculator.calculate` is missing".to_string(),
                "calculator.calculate(\"10 + 5\")".to_string(),
            ],
            requirement_refs: Vec::new(),
            source_refs: vec!["calculator.py".to_string(), "10 + 5".to_string()],
            test_refs: vec!["test_calculator.py".to_string()],
        }],
        sibling_obligations: vec![
            "`calculator.calculate` is missing".to_string(),
            "calculator.calculate(\"10 + 5\")".to_string(),
        ],
        source_refs: vec!["calculator.py".to_string(), "10 + 5".to_string()],
        test_refs: vec!["test_calculator.py".to_string()],
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

fn extract_tool_targets(meta: &ToolCallMeta, transcript: &Transcript) -> Vec<Utf8PathBuf> {
    match meta.tool_name {
        ToolName::Read | ToolName::InspectDirectory => {
            extract_json_string(&meta.arguments_json, "path")
                .into_iter()
                .filter_map(|value| normalize_target_path(&value, &transcript.session.cwd))
                .collect()
        }
        ToolName::Write => extract_json_string(&meta.arguments_json, "path")
            .into_iter()
            .filter_map(|value| normalize_target_path(&value, &transcript.session.cwd))
            .collect(),
        ToolName::ApplyPatch => extract_json_string(&meta.arguments_json, "patch_text")
            .into_iter()
            .flat_map(|value| extract_patch_targets(&value))
            .filter_map(|value| normalize_target_path(&value, &transcript.session.cwd))
            .collect(),
        _ => Vec::new(),
    }
}
fn extract_json_string(arguments_json: &str, key: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn extract_patch_targets(patch_text: &str) -> Vec<String> {
    patch_text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("*** Add File: ")
                .or_else(|| line.strip_prefix("*** Update File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .map(|value| value.trim().to_string())
        })
        .collect()
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
    let normalized = target.replace('\\', "/");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        return None;
    }
    let path = Utf8Path::new(trimmed);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    absolute
        .strip_prefix(workspace_root)
        .map(|value| value.to_path_buf())
        .ok()
        .or_else(|| (!path.is_absolute()).then(|| path.to_path_buf()))
}

fn merge_repair_targets(
    existing: Vec<Utf8PathBuf>,
    additional: Vec<Utf8PathBuf>,
) -> Vec<Utf8PathBuf> {
    let mut merged = existing;
    merged.extend(additional);
    prioritize_repair_targets(merged)
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

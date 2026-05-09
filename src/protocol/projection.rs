use serde_json::Value;
use sha2::{Digest, Sha256};
use std::str::FromStr;

use crate::protocol::{
    CandidateRepairId, CandidateRepairValidity, ContentPart, FileChangeEvidence, HistoryItem,
    HistoryItemId, HistoryItemPayload, PermissionDecision, ProjectionId, RuntimeEvent,
    RuntimeEventId, RuntimeEventMsg, SandboxDecision, SandboxProfile, ToolLifecycleEnvelope,
    ToolLifecycleStatus, ToolProgressEffect, ToolProposalId, TurnId, TurnItem, TurnItemId,
    TurnItemPayload, TurnTerminalStatus, VerificationRunResult,
};
use crate::runtime::SystemClock;
use crate::session::{RunEvent, SessionId};
use crate::tool::ToolName;

#[derive(Debug, Clone)]
pub struct ProtocolRunEventProjection {
    pub runtime_event: RuntimeEvent,
    pub history_item: Option<HistoryItem>,
    pub turn_item: Option<TurnItem>,
}

pub fn project_protocol_run_event(
    event: &RunEvent,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
    sequence_no: i64,
) -> Option<ProtocolRunEventProjection> {
    let session_id = session_id_for_run_event(event).or(fallback_session_id)?;
    let history_item = project_history_item_for_run_event(event, session_id, turn_id, sequence_no);
    let runtime_event = RuntimeEvent {
        id: RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: SystemClock::now_ms(),
        msg: runtime_msg_for_run_event(event, history_item.as_ref()),
    };
    let mut turn_item = project_turn_item_for_run_event(event, session_id, turn_id, sequence_no);
    if let (Some(turn_item), Some(history_item)) = (&mut turn_item, &history_item) {
        turn_item.source_item_id = Some(history_item.id);
    }
    Some(ProtocolRunEventProjection {
        runtime_event,
        history_item,
        turn_item,
    })
}

pub fn project_history_item_for_run_event(
    event: &RunEvent,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
) -> Option<HistoryItem> {
    let id = HistoryItemId::new();
    let payload = match event {
        RunEvent::TextDelta { message_id, delta } => HistoryItemPayload::Message {
            message_id: Some(*message_id),
            role: crate::session::MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: delta.clone(),
            }],
        },
        RunEvent::SessionFailed { message, .. } => HistoryItemPayload::Error {
            message_id: None,
            message: message.clone(),
        },
        RunEvent::ReasoningDelta { delta, .. } => HistoryItemPayload::Reasoning {
            text: delta.clone(),
        },
        RunEvent::ControlEnvelopePrepared { envelope, .. } => HistoryItemPayload::ControlEnvelope {
            envelope: envelope.clone(),
        },
        RunEvent::ModelRequestPrepared { diagnostics, .. } => {
            HistoryItemPayload::RequestDiagnostics {
                diagnostics: diagnostics.clone(),
            }
        }
        RunEvent::ToolCallPending {
            tool_call_id,
            tool,
            metadata,
            ..
        } => HistoryItemPayload::ToolCall {
            call_id: *tool_call_id,
            tool: tool.clone(),
            arguments: tool_effective_arguments_from_metadata(metadata),
            model_arguments: tool_model_arguments_from_metadata(metadata),
            effective_arguments: tool_effective_arguments_from_metadata(metadata),
            adjusted_arguments: tool_adjusted_arguments_from_metadata(metadata),
            permission_decision: permission_decision_from_metadata(metadata),
            sandbox_decision: sandbox_decision_from_metadata(metadata),
            allowed_surface: allowed_surface_from_metadata(metadata),
            retry_policy: tool_route_policy_from_metadata(metadata, "retry_policy"),
            terminal_guard_policy: tool_route_policy_from_metadata(
                metadata,
                "terminal_guard_policy",
            ),
        },
        RunEvent::ToolCallCompleted {
            tool_call_id,
            title,
            summary,
            metadata,
            ..
        } => HistoryItemPayload::ToolOutput {
            call_id: *tool_call_id,
            status: ToolLifecycleStatus::Completed,
            title: title.clone(),
            output_text: summary.clone(),
            metadata: metadata.clone(),
            success: tool_success_from_metadata(metadata, ToolLifecycleStatus::Completed),
            progress_effect: tool_progress_effect_from_metadata(
                metadata,
                ToolLifecycleStatus::Completed,
            ),
            blocked_action: blocked_action_from_metadata(metadata),
            required_next_action: required_next_action_from_metadata(metadata),
            result_hash: result_hash_from_metadata(metadata),
            verification_run: verification_run_result_from_metadata(metadata),
        },
        RunEvent::ToolCallFailed {
            tool_call_id,
            error,
            metadata,
            ..
        } => HistoryItemPayload::ToolOutput {
            call_id: *tool_call_id,
            status: ToolLifecycleStatus::Failed,
            title: "tool failed".to_string(),
            output_text: error.clone(),
            metadata: metadata.clone(),
            success: tool_success_from_metadata(metadata, ToolLifecycleStatus::Failed),
            progress_effect: tool_progress_effect_from_metadata(
                metadata,
                ToolLifecycleStatus::Failed,
            ),
            blocked_action: blocked_action_from_metadata(metadata),
            required_next_action: required_next_action_from_metadata(metadata),
            result_hash: result_hash_from_metadata(metadata),
            verification_run: verification_run_result_from_metadata(metadata),
        },
        RunEvent::ToolProposalRejected { proposal, .. } => {
            HistoryItemPayload::RejectedToolProposal {
                proposal: proposal.clone(),
            }
        }
        RunEvent::CandidateRepairEditRecorded { candidate, .. } => {
            HistoryItemPayload::CandidateRepairEdit {
                candidate: candidate.clone(),
            }
        }
        RunEvent::FileChangesRecorded { changes, .. } => HistoryItemPayload::FileChange {
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            changes: changes.iter().map(file_change_evidence).collect(),
            summary: changes
                .iter()
                .map(|change| change.summary_line(None))
                .collect::<Vec<_>>()
                .join("; "),
        },
        RunEvent::CompactionCompleted {
            summarized_messages,
            summary,
            continuation,
            ..
        } => HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::MidTurn,
            summary: if summary.trim().is_empty() {
                format!("summarized {summarized_messages} messages")
            } else {
                summary.clone()
            },
            replacement_item_ids: vec![id],
            continuation: continuation.clone(),
        },
        RunEvent::StateUpdated { state, .. } => HistoryItemPayload::SessionState {
            state: state.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            approved,
        } => HistoryItemPayload::ApprovalDecision {
            call_id: *tool_call_id,
            decision: if *approved {
                PermissionDecision::Approved
            } else {
                PermissionDecision::Denied {
                    reason: "permission denied by user".to_string(),
                }
            },
        },
        RunEvent::RetryScheduled {
            attempt,
            message,
            next_retry_at_ms,
            ..
        } => HistoryItemPayload::RetryDecision {
            attempt: *attempt,
            message: message.clone(),
            next_retry_at_ms: *next_retry_at_ms,
        },
        RunEvent::RecoverableRuntimeFeedback {
            message_id,
            message,
            ..
        } => HistoryItemPayload::Error {
            message_id: Some(*message_id),
            message: message.clone(),
        },
        RunEvent::UserTurnStored {
            message_id, turn, ..
        } => HistoryItemPayload::UserTurn {
            message_id: Some(*message_id),
            content: turn.content_parts(),
            prompt_dispatch: turn.prompt_dispatch.clone(),
            editor_context: turn.editor_context.clone(),
            turn_context: Some(Box::new(turn.context.clone())),
        },
        RunEvent::SessionStarted { .. }
        | RunEvent::UserMessageStored { .. }
        | RunEvent::AssistantStarted { .. }
        | RunEvent::PermissionRequested { .. }
        | RunEvent::SessionCompleted { .. }
        | RunEvent::SessionAwaitingUser { .. } => return None,
    };

    Some(HistoryItem {
        id,
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: SystemClock::now_ms(),
        payload,
    })
}

pub fn project_turn_item_for_run_event(
    event: &RunEvent,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
) -> Option<TurnItem> {
    let payload = match event {
        RunEvent::TextDelta { delta, .. } => TurnItemPayload::AgentMessage {
            text: delta.clone(),
        },
        RunEvent::ReasoningDelta { delta, .. } => TurnItemPayload::Reasoning {
            text: delta.clone(),
        },
        RunEvent::ToolCallPending {
            tool_call_id,
            tool,
            title,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Pending,
            title: title.clone(),
        },
        RunEvent::ToolCallCompleted {
            tool_call_id,
            tool,
            title,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Completed,
            title: title.clone(),
        },
        RunEvent::ToolCallFailed {
            tool_call_id,
            tool,
            error,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Failed,
            title: error.clone(),
        },
        RunEvent::ToolProposalRejected { .. } | RunEvent::CandidateRepairEditRecorded { .. } => {
            return None;
        }
        RunEvent::FileChangesRecorded { changes, .. } => TurnItemPayload::FileChange {
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            changes: changes.iter().map(file_change_evidence).collect(),
            summary: changes
                .iter()
                .map(|change| change.summary_line(None))
                .collect::<Vec<_>>()
                .join("; "),
        },
        RunEvent::CompactionCompleted {
            summarized_messages,
            summary,
            ..
        } => TurnItemPayload::ContextCompaction {
            summary: if summary.trim().is_empty() {
                format!("summarized {summarized_messages} messages")
            } else {
                summary.clone()
            },
        },
        RunEvent::PermissionRequested {
            tool_call_id,
            summary,
        } => TurnItemPayload::ApprovalRequest {
            call_id: *tool_call_id,
            summary: summary.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            approved,
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: ToolName::Shell,
            status: if *approved {
                ToolLifecycleStatus::Deferred
            } else {
                ToolLifecycleStatus::Rejected
            },
            title: if *approved {
                "Permission approved".to_string()
            } else {
                "Permission denied".to_string()
            },
        },
        RunEvent::SessionCompleted { .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Completed,
            summary: "session completed".to_string(),
        },
        RunEvent::SessionAwaitingUser { .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::AwaitingUser,
            summary: "session awaiting user".to_string(),
        },
        RunEvent::SessionFailed { message, .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Failed,
            summary: message.clone(),
        },
        RunEvent::RetryScheduled { message, .. } => TurnItemPayload::Warning {
            message: message.clone(),
        },
        RunEvent::RecoverableRuntimeFeedback { message, .. } => TurnItemPayload::Error {
            message: message.clone(),
        },
        RunEvent::StateUpdated { state, .. } => TurnItemPayload::State {
            summary: format!(
                "state projected: route={:?}, phase={:?}, active_targets={}",
                state.route,
                state.process_phase,
                state.active_targets.len()
            ),
        },
        RunEvent::UserTurnStored { turn, .. } => TurnItemPayload::UserMessage {
            text: turn
                .content_parts()
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => text.clone(),
                    ContentPart::Image { image } => image
                        .source_path
                        .as_ref()
                        .map(|path| format!("{path} ({} bytes)", image.byte_len))
                        .unwrap_or_else(|| format!("image attachment ({} bytes)", image.byte_len)),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        },
        RunEvent::UserMessageStored { .. }
        | RunEvent::SessionStarted { .. }
        | RunEvent::AssistantStarted { .. }
        | RunEvent::ControlEnvelopePrepared { .. }
        | RunEvent::ModelRequestPrepared { .. } => return None,
    };
    Some(TurnItem {
        id: TurnItemId::new(),
        session_id,
        turn_id,
        source_item_id: None,
        sequence_no,
        payload,
    })
}

fn runtime_msg_for_run_event(
    event: &RunEvent,
    history_item: Option<&HistoryItem>,
) -> RuntimeEventMsg {
    match event {
        RunEvent::SessionStarted { title, .. } => RuntimeEventMsg::Warning {
            message: format!("thread started: {title}"),
        },
        RunEvent::UserMessageStored { message_id } => RuntimeEventMsg::UserMessageStored {
            message_id: *message_id,
        },
        RunEvent::UserTurnStored { message_id, .. } => RuntimeEventMsg::UserMessageStored {
            message_id: *message_id,
        },
        RunEvent::AssistantStarted { message_id, model } => RuntimeEventMsg::AssistantStarted {
            message_id: *message_id,
            model: model.clone(),
        },
        RunEvent::ControlEnvelopePrepared { envelope, .. } => {
            RuntimeEventMsg::ControlEnvelopePrepared {
                envelope: envelope.clone(),
            }
        }
        RunEvent::ModelRequestPrepared { diagnostics, .. } => {
            RuntimeEventMsg::ModelRequestPrepared {
                diagnostics: diagnostics.clone(),
            }
        }
        RunEvent::TextDelta { message_id, delta } => RuntimeEventMsg::AssistantTextDelta {
            message_id: *message_id,
            delta: delta.clone(),
        },
        RunEvent::ReasoningDelta { message_id, delta } => RuntimeEventMsg::ReasoningDelta {
            message_id: *message_id,
            delta: delta.clone(),
        },
        RunEvent::ToolCallPending {
            tool_call_id,
            tool,
            title,
            metadata,
        } => {
            let mut envelope = tool_envelope(
                *tool_call_id,
                tool.clone(),
                ToolLifecycleStatus::Pending,
                None,
                None,
                Some(title.clone()),
            );
            apply_tool_route_metadata(&mut envelope, metadata);
            RuntimeEventMsg::ToolLifecycle { envelope }
        }
        RunEvent::ToolCallCompleted {
            tool_call_id,
            tool,
            title,
            summary,
            metadata,
            ..
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: completed_tool_envelope(
                *tool_call_id,
                tool.clone(),
                title,
                summary,
                metadata,
            ),
        },
        RunEvent::ToolCallFailed {
            tool_call_id,
            tool,
            error,
            metadata,
        } => {
            let mut envelope = tool_envelope(
                *tool_call_id,
                tool.clone(),
                ToolLifecycleStatus::Failed,
                Some(error),
                None,
                Some(error.clone()),
            );
            apply_completed_tool_metadata(&mut envelope, metadata);
            RuntimeEventMsg::ToolLifecycle { envelope }
        }
        RunEvent::ToolProposalRejected { proposal, .. } => RuntimeEventMsg::ToolProposalRejected {
            proposal: proposal.clone(),
        },
        RunEvent::CandidateRepairEditRecorded { candidate, .. } => {
            RuntimeEventMsg::CandidateRepairEditRecorded {
                candidate: candidate.clone(),
            }
        }
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => RuntimeEventMsg::FileChangesRecorded {
            call_id: *tool_call_id,
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            summary: changes
                .iter()
                .map(|change| change.summary_line(None))
                .collect::<Vec<_>>()
                .join("; "),
        },
        RunEvent::CompactionCompleted { .. } => RuntimeEventMsg::ContextCompacted {
            item_id: history_item
                .map(|item| item.id)
                .unwrap_or_else(crate::protocol::HistoryItemId::new),
            mode: crate::protocol::CompactionMode::MidTurn,
        },
        RunEvent::PermissionRequested {
            tool_call_id,
            summary,
        } => RuntimeEventMsg::ApprovalRequested {
            call_id: *tool_call_id,
            summary: summary.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            approved,
        } => RuntimeEventMsg::ApprovalResolved {
            call_id: *tool_call_id,
            decision: if *approved {
                PermissionDecision::Approved
            } else {
                PermissionDecision::Denied {
                    reason: "permission denied by user".to_string(),
                }
            },
        },
        RunEvent::RetryScheduled {
            attempt,
            message,
            next_retry_at_ms,
            ..
        } => RuntimeEventMsg::RetryScheduled {
            attempt: *attempt,
            message: message.clone(),
            next_retry_at_ms: *next_retry_at_ms,
        },
        RunEvent::RecoverableRuntimeFeedback { message, .. } => RuntimeEventMsg::Warning {
            message: message.clone(),
        },
        RunEvent::StateUpdated { state, .. } => RuntimeEventMsg::Warning {
            message: format!(
                "state projected: route={:?} phase={:?}",
                state.route, state.process_phase
            ),
        },
        RunEvent::SessionCompleted { finish_reason, .. } => RuntimeEventMsg::TurnCompleted {
            finish_reason: *finish_reason,
        },
        RunEvent::SessionAwaitingUser { finish_reason, .. } => RuntimeEventMsg::TurnAwaitingUser {
            reason: finish_reason
                .map(|reason| format!("{reason:?}"))
                .unwrap_or_else(|| "awaiting user".to_string()),
        },
        RunEvent::SessionFailed { message, .. } => RuntimeEventMsg::TurnFailed {
            message: message.clone(),
        },
    }
}

fn session_id_for_run_event(event: &RunEvent) -> Option<SessionId> {
    match event {
        RunEvent::SessionStarted { session_id, .. }
        | RunEvent::UserTurnStored { session_id, .. }
        | RunEvent::ControlEnvelopePrepared { session_id, .. }
        | RunEvent::ModelRequestPrepared { session_id, .. }
        | RunEvent::RetryScheduled { session_id, .. }
        | RunEvent::RecoverableRuntimeFeedback { session_id, .. }
        | RunEvent::StateUpdated { session_id, .. }
        | RunEvent::SessionCompleted { session_id, .. }
        | RunEvent::SessionAwaitingUser { session_id, .. }
        | RunEvent::SessionFailed { session_id, .. } => Some(*session_id),
        RunEvent::UserMessageStored { .. }
        | RunEvent::AssistantStarted { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ReasoningDelta { .. }
        | RunEvent::ToolCallPending { .. }
        | RunEvent::ToolCallCompleted { .. }
        | RunEvent::ToolCallFailed { .. }
        | RunEvent::ToolProposalRejected { .. }
        | RunEvent::CandidateRepairEditRecorded { .. }
        | RunEvent::FileChangesRecorded { .. }
        | RunEvent::CompactionCompleted { .. }
        | RunEvent::PermissionRequested { .. }
        | RunEvent::PermissionResolved { .. } => None,
    }
}

fn tool_envelope(
    call_id: crate::session::ToolCallId,
    tool: crate::tool::ToolName,
    status: ToolLifecycleStatus,
    hash_source: Option<&str>,
    required_next_action: Option<String>,
    blocked_action: Option<String>,
) -> ToolLifecycleEnvelope {
    ToolLifecycleEnvelope {
        call_id,
        tool: tool.clone(),
        proposal_id: None,
        candidate_repair_id: None,
        original_arguments: serde_json::Value::Null,
        adjusted_arguments: None,
        allowed_surface: vec![tool],
        permission_decision: PermissionDecision::NotRequired,
        sandbox_decision: SandboxDecision {
            profile: SandboxProfile::WorkspaceWrite,
            network_allowed: false,
            escalated: false,
        },
        status,
        rejection_reason: None,
        semantic_class: None,
        candidate_validity: None,
        result_hash: hash_source.map(|value| hash_text(value)),
        blocked_action,
        required_next_action,
        projection_id: ProjectionId::new(),
        contract_refs: vec!["thread_turn_item_protocol".to_string()],
        artifact_refs: Vec::new(),
    }
}

fn completed_tool_envelope(
    call_id: crate::session::ToolCallId,
    tool: crate::tool::ToolName,
    _title: &str,
    summary: &str,
    metadata: &Value,
) -> ToolLifecycleEnvelope {
    let mut envelope = tool_envelope(
        call_id,
        tool.clone(),
        ToolLifecycleStatus::Completed,
        Some(summary),
        None,
        None,
    );
    apply_completed_tool_metadata(&mut envelope, metadata);
    envelope
}

fn tool_model_arguments_from_metadata(metadata: &Value) -> Value {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("original_arguments"))
        .or_else(|| metadata.get("original_arguments"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn tool_effective_arguments_from_metadata(metadata: &Value) -> Value {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("effective_arguments"))
        .or_else(|| {
            metadata
                .get("tool_route")
                .and_then(|route| route.get("adjusted_arguments"))
        })
        .or_else(|| metadata.get("effective_arguments"))
        .or_else(|| metadata.get("adjusted_arguments"))
        .or_else(|| {
            metadata
                .get("tool_route")
                .and_then(|route| route.get("original_arguments"))
        })
        .or_else(|| metadata.get("original_arguments"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn tool_adjusted_arguments_from_metadata(metadata: &Value) -> Option<Value> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("adjusted_arguments"))
        .or_else(|| metadata.get("adjusted_arguments"))
        .filter(|value| !value.is_null())
        .cloned()
}

fn tool_route_policy_from_metadata(metadata: &Value, key: &str) -> Option<Value> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get(key))
        .or_else(|| metadata.get(key))
        .cloned()
}

fn permission_decision_from_metadata(metadata: &Value) -> Option<PermissionDecision> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("permission_decision"))
        .or_else(|| metadata.get("permission_decision"))
        .and_then(permission_decision_from_value)
}

fn sandbox_decision_from_metadata(metadata: &Value) -> Option<SandboxDecision> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("sandbox_decision"))
        .or_else(|| metadata.get("sandbox_decision"))
        .and_then(sandbox_decision_from_value)
}

fn allowed_surface_from_metadata(metadata: &Value) -> Vec<ToolName> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("allowed_tools"))
        .or_else(|| metadata.get("allowed_tools"))
        .and_then(tool_surface_from_value)
        .unwrap_or_default()
}

fn verification_run_result_from_metadata(metadata: &Value) -> Option<VerificationRunResult> {
    metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
}

fn tool_success_from_metadata(metadata: &Value, status: ToolLifecycleStatus) -> Option<bool> {
    metadata
        .get("success")
        .or_else(|| {
            metadata
                .get("tool_feedback_envelope")
                .and_then(|feedback| feedback.get("success"))
        })
        .and_then(Value::as_bool)
        .or_else(|| match status {
            ToolLifecycleStatus::Completed => Some(true),
            ToolLifecycleStatus::Failed
            | ToolLifecycleStatus::Blocked
            | ToolLifecycleStatus::Rejected => Some(false),
            _ => None,
        })
}

fn tool_progress_effect_from_metadata(
    metadata: &Value,
    status: ToolLifecycleStatus,
) -> ToolProgressEffect {
    if let Some(run) = verification_run_result_from_metadata(metadata) {
        return match run.status {
            crate::protocol::VerificationRunStatus::Passed => {
                ToolProgressEffect::VerificationPassed
            }
            crate::protocol::VerificationRunStatus::Failed
            | crate::protocol::VerificationRunStatus::TimedOut => {
                ToolProgressEffect::VerificationFailed
            }
            crate::protocol::VerificationRunStatus::NotVerification => ToolProgressEffect::Unknown,
        };
    }
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str)
        .map(|value| match value {
            "made_progress" | "progress" => ToolProgressEffect::MadeProgress,
            "no_progress" => ToolProgressEffect::NoProgress,
            "blocked" => ToolProgressEffect::Blocked,
            "verification_passed" => ToolProgressEffect::VerificationPassed,
            "verification_failed" => ToolProgressEffect::VerificationFailed,
            _ => ToolProgressEffect::Unknown,
        })
        .unwrap_or_else(|| match status {
            ToolLifecycleStatus::Completed => ToolProgressEffect::MadeProgress,
            ToolLifecycleStatus::Failed
            | ToolLifecycleStatus::Blocked
            | ToolLifecycleStatus::Rejected => ToolProgressEffect::Blocked,
            _ => ToolProgressEffect::Unknown,
        })
}

fn required_next_action_from_metadata(metadata: &Value) -> Option<String> {
    let _ = metadata;
    None
}

fn blocked_action_from_metadata(metadata: &Value) -> Option<String> {
    let _ = metadata;
    None
}

fn result_hash_from_metadata(metadata: &Value) -> Option<String> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("result_hash"))
        .or_else(|| metadata.get("result_hash"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn file_change_evidence(change: &crate::edit::ChangeSummary) -> FileChangeEvidence {
    FileChangeEvidence {
        change_id: change.change_id,
        kind: change.kind,
        path_before: change.path_before.clone(),
        path_after: change.path_after.clone(),
        summary: change.summary_line(None),
    }
}

fn apply_completed_tool_metadata(envelope: &mut ToolLifecycleEnvelope, metadata: &Value) {
    apply_tool_route_metadata(envelope, metadata);
    let control_projection = metadata.get("control_projection");
    let feedback_envelope = metadata.get("tool_feedback_envelope");

    if let Some(projection_id) = control_projection
        .and_then(|projection| projection.get("projection_id"))
        .and_then(projection_id_from_value)
    {
        envelope.projection_id = projection_id;
    }

    if let Some(allowed_surface) = control_projection
        .and_then(|projection| projection.get("allowed_tools"))
        .and_then(tool_surface_from_value)
        .filter(|tools| !tools.is_empty())
        .or_else(|| {
            feedback_envelope
                .and_then(|feedback| feedback.get("allowed_surface_snapshot"))
                .and_then(tool_surface_from_value)
                .filter(|tools| !tools.is_empty())
        })
    {
        envelope.allowed_surface = allowed_surface;
    }

    if let Some(result_hash) = feedback_envelope
        .and_then(|feedback| feedback.get("result_hash"))
        .and_then(string_from_value)
    {
        envelope.result_hash = Some(result_hash);
    }

    if let Some(progress_class) = feedback_envelope
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(string_from_value)
    {
        envelope.semantic_class = Some(progress_class);
    }

    apply_candidate_repair_metadata(envelope, metadata);
    add_metadata_contract_refs(envelope, metadata);
}

fn apply_tool_route_metadata(envelope: &mut ToolLifecycleEnvelope, metadata: &Value) {
    let route = metadata.get("tool_route").unwrap_or(metadata);

    if let Some(original_arguments) = route
        .get("original_arguments")
        .or_else(|| metadata.get("original_arguments"))
        .cloned()
    {
        envelope.original_arguments = original_arguments;
    }

    if let Some(adjusted_arguments) = route
        .get("adjusted_arguments")
        .or_else(|| metadata.get("adjusted_arguments"))
        .filter(|value| !value.is_null())
        .cloned()
    {
        envelope.adjusted_arguments = Some(adjusted_arguments);
    }

    if let Some(allowed_surface) = route
        .get("allowed_tools")
        .or_else(|| metadata.get("allowed_tools"))
        .and_then(tool_surface_from_value)
        .filter(|tools| !tools.is_empty())
    {
        envelope.allowed_surface = allowed_surface;
    }

    if let Some(permission) = route
        .get("permission_decision")
        .or_else(|| metadata.get("permission_decision"))
        .and_then(permission_decision_from_value)
    {
        envelope.permission_decision = permission;
    }

    if let Some(sandbox) = route
        .get("sandbox_decision")
        .or_else(|| metadata.get("sandbox_decision"))
        .and_then(sandbox_decision_from_value)
    {
        envelope.sandbox_decision = sandbox;
    }

    if metadata.get("tool_route").is_some() {
        envelope
            .contract_refs
            .push("tool_route_decision".to_string());
        if route.get("retry_policy").is_some() {
            envelope
                .contract_refs
                .push("tool_orchestrator_retry_policy".to_string());
        }
        if route.get("terminal_guard_policy").is_some() {
            envelope
                .contract_refs
                .push("tool_orchestrator_terminal_guard_policy".to_string());
        }
        envelope.contract_refs.sort();
        envelope.contract_refs.dedup();
    }
}

fn permission_decision_from_value(value: &Value) -> Option<PermissionDecision> {
    match value.as_str()? {
        "not_required" => Some(PermissionDecision::NotRequired),
        "pending" => Some(PermissionDecision::Pending),
        "approved" => Some(PermissionDecision::Approved),
        "denied" => Some(PermissionDecision::Denied {
            reason: "tool route denied".to_string(),
        }),
        _ => None,
    }
}

fn sandbox_decision_from_value(value: &Value) -> Option<SandboxDecision> {
    let profile = match value.get("profile").and_then(Value::as_str)? {
        "read_only" => SandboxProfile::ReadOnly,
        "workspace_write" => SandboxProfile::WorkspaceWrite,
        "full_access" => SandboxProfile::FullAccess,
        _ => return None,
    };
    Some(SandboxDecision {
        profile,
        network_allowed: value
            .get("network_allowed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        escalated: value
            .get("escalated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn apply_candidate_repair_metadata(envelope: &mut ToolLifecycleEnvelope, metadata: &Value) {
    let proposal = metadata.get("rejected_tool_proposal");
    let candidate = metadata.get("candidate_repair_edit");

    if let Some(proposal_id) = proposal
        .and_then(|value| value.get("proposal_id"))
        .and_then(tool_proposal_id_from_value)
    {
        envelope.proposal_id = Some(proposal_id);
        envelope.status = ToolLifecycleStatus::Rejected;
    }

    if let Some(candidate_id) = candidate
        .and_then(|value| value.get("candidate_id"))
        .and_then(candidate_repair_id_from_value)
        .or_else(|| {
            proposal
                .and_then(|value| value.get("candidate_repair_id"))
                .and_then(candidate_repair_id_from_value)
        })
    {
        envelope.candidate_repair_id = Some(candidate_id);
    }

    if let Some(reason) = proposal
        .and_then(|value| value.get("blocked_reason"))
        .and_then(string_from_value)
    {
        envelope.rejection_reason = Some(reason);
    }

    if let Some(semantic_class) = candidate
        .and_then(|value| value.get("semantic_class"))
        .or_else(|| proposal.and_then(|value| value.get("semantic_class")))
        .and_then(string_from_value)
    {
        envelope.semantic_class = Some(semantic_class);
    }

    if let Some(validity) = candidate
        .and_then(|value| value.get("validity"))
        .and_then(candidate_validity_from_value)
    {
        envelope.candidate_validity = Some(validity);
    }
}

fn projection_id_from_value(value: &Value) -> Option<ProjectionId> {
    value
        .as_str()
        .and_then(|value| ProjectionId::from_str(value).ok())
}

fn tool_proposal_id_from_value(value: &Value) -> Option<ToolProposalId> {
    value
        .as_str()
        .and_then(|value| ToolProposalId::from_str(value).ok())
}

fn candidate_repair_id_from_value(value: &Value) -> Option<CandidateRepairId> {
    value
        .as_str()
        .and_then(|value| CandidateRepairId::from_str(value).ok())
}

fn candidate_validity_from_value(value: &Value) -> Option<CandidateRepairValidity> {
    match value.as_str()? {
        "unverified" => Some(CandidateRepairValidity::Unverified),
        "tentative" => Some(CandidateRepairValidity::Tentative),
        "contract_delta_verified" => Some(CandidateRepairValidity::ContractDeltaVerified),
        "admitted" => Some(CandidateRepairValidity::Admitted),
        "contradicted" => Some(CandidateRepairValidity::Contradicted),
        "rejected" => Some(CandidateRepairValidity::Rejected),
        "superseded" => Some(CandidateRepairValidity::Superseded),
        "expired" => Some(CandidateRepairValidity::Expired),
        "unsafe" => Some(CandidateRepairValidity::Unsafe),
        _ => None,
    }
}

fn string_from_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "none")
        .map(str::to_string)
}

fn tool_surface_from_value(value: &Value) -> Option<Vec<crate::tool::ToolName>> {
    let array = value.as_array()?;
    let mut tools = array
        .iter()
        .filter_map(Value::as_str)
        .filter_map(tool_name_from_token)
        .collect::<Vec<_>>();
    tools.sort_by_key(|tool| tool.to_string());
    tools.dedup();
    Some(tools)
}

fn tool_name_from_token(token: &str) -> Option<crate::tool::ToolName> {
    match token {
        "list" => Some(crate::tool::ToolName::List),
        "glob" => Some(crate::tool::ToolName::Glob),
        "grep" => Some(crate::tool::ToolName::Grep),
        "read" => Some(crate::tool::ToolName::Read),
        "inspect_directory" => Some(crate::tool::ToolName::InspectDirectory),
        "apply_patch" => Some(crate::tool::ToolName::ApplyPatch),
        "write" => Some(crate::tool::ToolName::Write),
        "shell" => Some(crate::tool::ToolName::Shell),
        "skill" => Some(crate::tool::ToolName::Skill),
        "docling_convert" => Some(crate::tool::ToolName::DoclingConvert),
        "mcp_call" => Some(crate::tool::ToolName::McpCall),
        "todowrite" => Some(crate::tool::ToolName::TodoWrite),
        _ => None,
    }
}

fn add_metadata_contract_refs(envelope: &mut ToolLifecycleEnvelope, metadata: &Value) {
    let mut refs = envelope.contract_refs.clone();
    if metadata.get("control_projection").is_some() {
        refs.push("turn_control_envelope".to_string());
        refs.push("projection_bundle".to_string());
    }
    if let Some(feedback) = metadata.get("tool_feedback_envelope") {
        refs.push("tool_feedback_envelope".to_string());
        if feedback.get("repair_operation_template").is_some() {
            refs.push("repair_operation_template".to_string());
        }
        if feedback.get("verification_cluster").is_some() {
            refs.push("verification_failure_cluster".to_string());
        }
        if feedback.get("repair_control_snapshot").is_some() {
            refs.push("repair_control_snapshot".to_string());
        }
        if feedback.get("contract_reconciliation").is_some() {
            refs.push("contract_reconciliation".to_string());
        }
    }
    if metadata.get("rejected_tool_proposal").is_some() {
        refs.push("rejected_tool_proposal".to_string());
    }
    if metadata.get("candidate_repair_edit").is_some() {
        refs.push("candidate_repair_edit".to_string());
    }
    refs.sort();
    refs.dedup();
    envelope.contract_refs = refs;
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

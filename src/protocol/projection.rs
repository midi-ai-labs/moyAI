use serde_json::Value;
use sha2::{Digest, Sha256};
use std::str::FromStr;

use crate::protocol::{
    CandidateRepairId, CandidateRepairValidity, ContentPart, FileChangeEvidence, HistoryItem,
    HistoryItemId, HistoryItemPayload, InterAgentCommunication, PermissionDecision, ProjectionId,
    RuntimeEvent, RuntimeEventId, RuntimeEventMsg, SandboxDecision, SandboxProfile,
    SubAgentActivityKind, ToolLifecycleEnvelope, ToolLifecycleStatus, ToolProgressEffect,
    ToolProposalId, TurnId, TurnItem, TurnItemId, TurnItemPayload, TurnTerminalStatus,
    VerificationRunResult,
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

pub fn project_inter_agent_communication(
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
    communication: InterAgentCommunication,
) -> ProtocolRunEventProjection {
    let created_at_ms = SystemClock::now_ms();
    let history_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms,
        payload: HistoryItemPayload::InterAgentCommunication {
            communication: communication.clone(),
        },
    };
    let turn_item = TurnItem {
        id: TurnItemId::new(),
        session_id,
        turn_id,
        source_item_id: Some(history_item.id),
        sequence_no,
        payload: TurnItemPayload::InterAgentCommunication {
            communication: communication.clone(),
        },
    };
    ProtocolRunEventProjection {
        runtime_event: RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms,
            msg: RuntimeEventMsg::InterAgentCommunicationReceived { communication },
        },
        history_item: Some(history_item),
        turn_item: Some(turn_item),
    }
}

pub fn project_sub_agent_activity(
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
    activity_id: String,
    agent_session_id: SessionId,
    agent_path: String,
    activity_kind: SubAgentActivityKind,
) -> ProtocolRunEventProjection {
    let created_at_ms = SystemClock::now_ms();
    let history_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms,
        payload: HistoryItemPayload::SubAgentActivity {
            activity_id: activity_id.clone(),
            agent_session_id,
            agent_path: agent_path.clone(),
            activity_kind,
        },
    };
    let turn_item = TurnItem {
        id: TurnItemId::new(),
        session_id,
        turn_id,
        source_item_id: Some(history_item.id),
        sequence_no,
        payload: TurnItemPayload::SubAgentActivity {
            activity_id: activity_id.clone(),
            agent_session_id,
            agent_path: agent_path.clone(),
            activity_kind,
        },
    };
    ProtocolRunEventProjection {
        runtime_event: RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms,
            msg: RuntimeEventMsg::SubAgentActivity {
                activity_id,
                agent_session_id,
                agent_path,
                activity_kind,
            },
        },
        history_item: Some(history_item),
        turn_item: Some(turn_item),
    }
}

pub fn filechange_item_projection_preserves_call_id_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let tool_call_id = crate::session::ToolCallId::new();
    let change_id = crate::session::ChangeId::new();
    let event = RunEvent::FileChangesRecorded {
        tool_call_id,
        changes: vec![crate::edit::ChangeSummary {
            change_id,
            kind: crate::session::ChangeKind::Update,
            path_before: Some(camino::Utf8PathBuf::from("src/lib.rs")),
            path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
        }],
    };
    let Some(projection) = project_protocol_run_event(&event, Some(session_id), turn_id, 7) else {
        return false;
    };
    let runtime_has_owner = matches!(
        projection.runtime_event.msg,
        RuntimeEventMsg::FileChangesRecorded {
            call_id,
            ref change_ids,
            ..
        } if call_id == tool_call_id && change_ids.as_slice() == [change_id]
    );
    let history_has_owner = matches!(
        projection.history_item.as_ref().map(|item| &item.payload),
        Some(HistoryItemPayload::FileChange {
            call_id,
            change_ids,
            ..
        }) if *call_id == tool_call_id && change_ids.as_slice() == [change_id]
    );
    let turn_item_has_owner = matches!(
        projection.turn_item.as_ref().map(|item| &item.payload),
        Some(TurnItemPayload::FileChange {
            call_id,
            change_ids,
            ..
        }) if *call_id == tool_call_id && change_ids.as_slice() == [change_id]
    );
    runtime_has_owner && history_has_owner && turn_item_has_owner
}

pub fn tool_output_projection_preserves_blocked_action_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let tool_call_id = crate::session::ToolCallId::new();
    let metadata = serde_json::json!({
        "success": false,
        "progress_effect": "no_progress",
        "tool_feedback_envelope": {
            "blocked_action": "apply_patch:src/workflow.rs",
            "required_next_action": "apply_patch:src/workflow.rs",
            "result_hash": "blocked-action-hash",
            "operation_progress_class": "wrong_target_authoring_edit"
        }
    });
    let event = RunEvent::ToolCallCompleted {
        tool_call_id,
        tool: ToolName::ApplyPatch,
        title: "Wrong target rejected".to_string(),
        summary: "The submitted edit targeted an inactive artifact.".to_string(),
        metadata,
    };
    let Some(projection) = project_protocol_run_event(&event, Some(session_id), turn_id, 8) else {
        return false;
    };
    let history_preserves_blocked_action = matches!(
        projection.history_item.as_ref().map(|item| &item.payload),
        Some(HistoryItemPayload::ToolOutput {
            blocked_action,
            result_hash,
            ..
        }) if blocked_action.as_deref() == Some("apply_patch:src/workflow.rs")
            && result_hash.as_deref() == Some("blocked-action-hash")
    );
    let runtime_preserves_blocked_action = matches!(
        projection.runtime_event.msg,
        RuntimeEventMsg::ToolLifecycle { ref envelope }
            if envelope.blocked_action.as_deref() == Some("apply_patch:src/workflow.rs")
                && envelope.result_hash.as_deref() == Some("blocked-action-hash")
                && envelope.semantic_class.as_deref() == Some("wrong_target_authoring_edit")
    );
    history_preserves_blocked_action && runtime_preserves_blocked_action
}

pub fn pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let tool_call_id = crate::session::ToolCallId::new();
    let event = RunEvent::ToolCallPending {
        tool_call_id,
        tool: ToolName::ApplyPatch,
        title: "apply patch src/workflow.rs".to_string(),
        metadata: serde_json::json!({
            "tool_route": {
                "original_arguments": {"path": "src/workflow.rs"},
                "effective_arguments": {"path": "src/workflow.rs"},
                "allowed_tools": ["apply_patch"]
            }
        }),
    };
    let Some(projection) = project_protocol_run_event(&event, Some(session_id), turn_id, 6) else {
        return false;
    };
    let runtime_pending_has_no_blocked_action = matches!(
        projection.runtime_event.msg,
        RuntimeEventMsg::ToolLifecycle { ref envelope }
            if envelope.status == ToolLifecycleStatus::Pending
                && envelope.blocked_action.is_none()
                && envelope.result_hash.is_none()
                && envelope.allowed_surface == vec![ToolName::ApplyPatch]
    );
    let history_pending_is_tool_call_not_output = matches!(
        projection.history_item.as_ref().map(|item| &item.payload),
        Some(HistoryItemPayload::ToolCall {
            call_id,
            tool,
            effective_arguments,
            ..
        }) if *call_id == tool_call_id
            && *tool == ToolName::ApplyPatch
            && effective_arguments
                .get("path")
                .and_then(Value::as_str)
                == Some("src/workflow.rs")
    );
    runtime_pending_has_no_blocked_action && history_pending_is_tool_call_not_output
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
        RunEvent::SessionInterrupted { .. } => return None,
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
        RunEvent::WorldStateUpdated {
            snapshot, rendered, ..
        } => HistoryItemPayload::WorldState {
            snapshot: snapshot.clone(),
            rendered: rendered.clone(),
        },
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
            result_hash: result_hash_from_metadata(metadata),
            verification_run: verification_run_result_from_metadata(metadata),
        },
        RunEvent::ToolCallDeclined {
            tool_call_id,
            reason,
            metadata,
            ..
        } => HistoryItemPayload::ToolOutput {
            call_id: *tool_call_id,
            status: ToolLifecycleStatus::Declined,
            title: "tool declined".to_string(),
            output_text: reason.clone(),
            metadata: metadata.clone(),
            success: None,
            progress_effect: ToolProgressEffect::Unknown,
            blocked_action: None,
            result_hash: None,
            verification_run: None,
        },
        RunEvent::ToolCallCancelled {
            tool_call_id,
            reason,
            metadata,
            ..
        } => HistoryItemPayload::ToolOutput {
            call_id: *tool_call_id,
            status: ToolLifecycleStatus::Cancelled,
            title: "tool cancelled".to_string(),
            output_text: reason.clone(),
            metadata: metadata.clone(),
            success: None,
            progress_effect: ToolProgressEffect::Unknown,
            blocked_action: None,
            result_hash: None,
            verification_run: None,
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
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => HistoryItemPayload::FileChange {
            call_id: *tool_call_id,
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
            replacement_item_ids,
            continuation,
            ..
        } => HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::MidTurn,
            summary: if summary.trim().is_empty() {
                format!("summarized {summarized_messages} messages")
            } else {
                summary.clone()
            },
            replacement_item_ids: replacement_item_ids.clone(),
            continuation: continuation.clone(),
        },
        RunEvent::StateUpdated { state, .. } => HistoryItemPayload::SessionState {
            state: state.clone(),
        },
        RunEvent::LifecycleGuardUpdated { snapshot, .. } => HistoryItemPayload::LifecycleGuard {
            snapshot: snapshot.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            tool: _,
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
        | RunEvent::SessionTitleUpdated { .. }
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
            summary: String::new(),
        },
        RunEvent::ToolCallCompleted {
            tool_call_id,
            tool,
            title,
            summary,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Completed,
            title: title.clone(),
            summary: summary.clone(),
        },
        RunEvent::ToolCallDeclined {
            tool_call_id,
            tool,
            reason,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Declined,
            title: "Tool declined".to_string(),
            summary: reason.clone(),
        },
        RunEvent::ToolCallCancelled {
            tool_call_id,
            tool,
            reason,
            ..
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: tool.clone(),
            status: ToolLifecycleStatus::Cancelled,
            title: "Tool cancelled".to_string(),
            summary: reason.clone(),
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
            summary: error.clone(),
        },
        RunEvent::ToolProposalRejected { .. } | RunEvent::CandidateRepairEditRecorded { .. } => {
            return None;
        }
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => TurnItemPayload::FileChange {
            call_id: *tool_call_id,
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
            tool: _,
            summary,
        } => TurnItemPayload::ApprovalRequest {
            call_id: *tool_call_id,
            summary: summary.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            tool,
            approved,
        } => TurnItemPayload::ToolStatus {
            call_id: *tool_call_id,
            tool: *tool,
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
            summary: String::new(),
        },
        RunEvent::SessionCompleted { .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Completed,
            summary: "session completed".to_string(),
            cause: None,
        },
        RunEvent::SessionAwaitingUser { .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::AwaitingUser,
            summary: "session awaiting user".to_string(),
            cause: None,
        },
        RunEvent::SessionFailed { message, .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Failed,
            summary: message.clone(),
            cause: None,
        },
        RunEvent::SessionInterrupted { reason, cause, .. } => TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Interrupted,
            summary: reason.clone(),
            cause: *cause,
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
        RunEvent::LifecycleGuardUpdated { snapshot, .. } => TurnItemPayload::LifecycleGuard {
            summary: lifecycle_guard_summary(snapshot),
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
        | RunEvent::SessionTitleUpdated { .. }
        | RunEvent::AssistantStarted { .. }
        | RunEvent::ControlEnvelopePrepared { .. }
        | RunEvent::ModelRequestPrepared { .. } => return None,
        RunEvent::WorldStateUpdated { snapshot, .. } => TurnItemPayload::WorldState {
            summary: format!("world state updated: {} sections", snapshot.section_count()),
        },
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
        RunEvent::SessionTitleUpdated { title, .. } => RuntimeEventMsg::Warning {
            message: format!("thread title updated: {title}"),
        },
        RunEvent::UserMessageStored { .. } => RuntimeEventMsg::UserInputAccepted { item_count: 1 },
        RunEvent::UserTurnStored { turn, .. } => RuntimeEventMsg::TurnStarted {
            context: turn.context.clone(),
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
        RunEvent::WorldStateUpdated { snapshot, .. } => RuntimeEventMsg::WorldStateUpdated {
            snapshot: snapshot.clone(),
        },
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
            title: _,
            metadata,
        } => {
            let mut envelope = tool_envelope(
                *tool_call_id,
                tool.clone(),
                ToolLifecycleStatus::Pending,
                None,
                None,
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
        RunEvent::ToolCallDeclined {
            tool_call_id,
            tool,
            reason,
            metadata,
        } => {
            let mut envelope = tool_envelope(
                *tool_call_id,
                tool.clone(),
                ToolLifecycleStatus::Declined,
                Some(reason),
                Some(reason.clone()),
            );
            apply_completed_tool_metadata(&mut envelope, metadata);
            RuntimeEventMsg::ToolLifecycle { envelope }
        }
        RunEvent::ToolCallCancelled {
            tool_call_id,
            tool,
            reason,
            metadata,
        } => {
            let mut envelope = tool_envelope(
                *tool_call_id,
                tool.clone(),
                ToolLifecycleStatus::Cancelled,
                Some(reason),
                Some(reason.clone()),
            );
            apply_completed_tool_metadata(&mut envelope, metadata);
            RuntimeEventMsg::ToolLifecycle { envelope }
        }
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
            tool,
            summary,
        } => RuntimeEventMsg::ApprovalRequested {
            call_id: *tool_call_id,
            tool: *tool,
            summary: summary.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            tool,
            approved,
        } => RuntimeEventMsg::ApprovalResolved {
            call_id: *tool_call_id,
            tool: *tool,
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
        RunEvent::LifecycleGuardUpdated { snapshot, .. } => {
            RuntimeEventMsg::LifecycleGuardUpdated {
                snapshot: snapshot.clone(),
            }
        }
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
        RunEvent::SessionInterrupted { reason, cause, .. } => RuntimeEventMsg::TurnInterrupted {
            reason: reason.clone(),
            cause: *cause,
        },
    }
}

fn lifecycle_guard_summary(snapshot: &crate::protocol::LifecycleGuardSnapshot) -> String {
    let counter_count = snapshot.counters.len();
    let flag_count = snapshot.active_flags.len();
    let target_count = snapshot.scoped_targets.len();
    format!(
        "lifecycle guard projected: counters={counter_count}, active_flags={flag_count}, scoped_targets={target_count}"
    )
}

fn session_id_for_run_event(event: &RunEvent) -> Option<SessionId> {
    match event {
        RunEvent::SessionStarted { session_id, .. }
        | RunEvent::SessionTitleUpdated { session_id, .. }
        | RunEvent::UserTurnStored { session_id, .. }
        | RunEvent::ControlEnvelopePrepared { session_id, .. }
        | RunEvent::ModelRequestPrepared { session_id, .. }
        | RunEvent::WorldStateUpdated { session_id, .. }
        | RunEvent::RetryScheduled { session_id, .. }
        | RunEvent::RecoverableRuntimeFeedback { session_id, .. }
        | RunEvent::StateUpdated { session_id, .. }
        | RunEvent::LifecycleGuardUpdated { session_id, .. }
        | RunEvent::SessionCompleted { session_id, .. }
        | RunEvent::SessionAwaitingUser { session_id, .. }
        | RunEvent::SessionInterrupted { session_id, .. }
        | RunEvent::SessionFailed { session_id, .. } => Some(*session_id),
        RunEvent::UserMessageStored { .. }
        | RunEvent::AssistantStarted { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ReasoningDelta { .. }
        | RunEvent::ToolCallPending { .. }
        | RunEvent::ToolCallCompleted { .. }
        | RunEvent::ToolCallDeclined { .. }
        | RunEvent::ToolCallCancelled { .. }
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

fn blocked_action_from_metadata(metadata: &Value) -> Option<String> {
    let feedback = metadata.get("tool_feedback_envelope");
    feedback
        .and_then(|value| value.get("blocked_action"))
        .or_else(|| feedback.and_then(|value| value.get("required_next_action")))
        .or_else(|| metadata.get("blocked_action"))
        .or_else(|| metadata.get("required_next_action"))
        .and_then(string_from_value)
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

    if let Some(blocked_action) = blocked_action_from_metadata(metadata) {
        envelope.blocked_action = Some(blocked_action);
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
        "current_time" => Some(crate::tool::ToolName::CurrentTime),
        "skill" => Some(crate::tool::ToolName::Skill),
        "docling_convert" => Some(crate::tool::ToolName::DoclingConvert),
        "mcp_call" => Some(crate::tool::ToolName::McpCall),
        "update_plan" | "todowrite" | "todo_write" => Some(crate::tool::ToolName::UpdatePlan),
        "get_goal" => Some(crate::tool::ToolName::GetGoal),
        "create_goal" => Some(crate::tool::ToolName::CreateGoal),
        "update_goal" => Some(crate::tool::ToolName::UpdateGoal),
        "spawn_agent" => Some(crate::tool::ToolName::SpawnAgent),
        "send_message" => Some(crate::tool::ToolName::SendMessage),
        "followup_task" => Some(crate::tool::ToolName::FollowupTask),
        "wait_agent" => Some(crate::tool::ToolName::WaitAgent),
        "interrupt_agent" => Some(crate::tool::ToolName::InterruptAgent),
        "list_agents" => Some(crate::tool::ToolName::ListAgents),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        HistoryItemAuthorityRole, RuntimeEventMsg, TurnInterruptionCause, TurnItemProjectionRole,
        TurnTerminalStatus,
    };
    use crate::session::{RunEvent, SessionId};

    #[test]
    fn plan_tool_tokens_project_to_the_canonical_identity() {
        for token in ["update_plan", "todowrite", "todo_write"] {
            assert_eq!(
                tool_name_from_token(token),
                Some(crate::tool::ToolName::UpdatePlan)
            );
        }
    }

    #[test]
    fn session_interrupted_projects_to_interrupted_terminal_items() {
        let session_id = SessionId::new();
        let projection = project_protocol_run_event(
            &RunEvent::SessionInterrupted {
                session_id,
                reason: "run cancelled by user".to_string(),
                cause: Some(TurnInterruptionCause::UserStop),
            },
            None,
            TurnId::new(),
            42,
        )
        .expect("interrupted event should project");

        match projection.runtime_event.msg {
            RuntimeEventMsg::TurnInterrupted { reason, cause } => {
                assert_eq!(reason, "run cancelled by user");
                assert_eq!(cause, Some(TurnInterruptionCause::UserStop));
            }
            other => panic!("unexpected runtime event: {other:?}"),
        }
        match projection.turn_item.expect("terminal turn item").payload {
            TurnItemPayload::Terminal {
                status,
                summary,
                cause,
            } => {
                assert_eq!(status, TurnTerminalStatus::Interrupted);
                assert_eq!(summary, "run cancelled by user");
                assert_eq!(cause, Some(TurnInterruptionCause::UserStop));
            }
            other => panic!("unexpected turn item: {other:?}"),
        }
        assert!(
            projection.history_item.is_none(),
            "an interruption is typed terminal state, not a history error"
        );
    }

    #[test]
    fn inter_agent_communication_projects_as_visible_assistant_output() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let communication = InterAgentCommunication {
            author: "/root/reviewer".to_string(),
            recipient: "/root".to_string(),
            content: "review complete".to_string(),
            trigger_turn: false,
        };

        let projection =
            project_inter_agent_communication(session_id, turn_id, 7, communication.clone());
        assert!(matches!(
            &projection.runtime_event.msg,
            RuntimeEventMsg::InterAgentCommunicationReceived { communication: stored }
                if stored == &communication
        ));
        let history = projection.history_item.expect("history item");
        assert_eq!(
            history.payload.authority_role(),
            HistoryItemAuthorityRole::AssistantOutput
        );
        let turn = projection.turn_item.expect("turn item");
        assert_eq!(turn.source_item_id, Some(history.id));
        assert_eq!(
            turn.payload.projection_role(),
            TurnItemProjectionRole::AssistantVisibleMessage
        );
    }

    #[test]
    fn sub_agent_activity_projects_as_runtime_control() {
        let session_id = SessionId::new();
        let agent_session_id = SessionId::new();
        let turn_id = TurnId::new();

        let projection = project_sub_agent_activity(
            session_id,
            turn_id,
            8,
            "activity-1".to_string(),
            agent_session_id,
            "/root/reviewer".to_string(),
            SubAgentActivityKind::Started,
        );
        assert!(matches!(
            &projection.runtime_event.msg,
            RuntimeEventMsg::SubAgentActivity {
                activity_id,
                agent_session_id: stored_session_id,
                agent_path,
                activity_kind: SubAgentActivityKind::Started,
            } if activity_id == "activity-1"
                && stored_session_id == &agent_session_id
                && agent_path == "/root/reviewer"
        ));
        let history = projection.history_item.expect("history item");
        assert_eq!(
            history.payload.authority_role(),
            HistoryItemAuthorityRole::RuntimeControl
        );
        let turn = projection.turn_item.expect("turn item");
        assert_eq!(turn.source_item_id, Some(history.id));
        assert_eq!(
            turn.payload.projection_role(),
            TurnItemProjectionRole::RuntimeControl
        );
        assert!(turn.payload.is_internal_projection_only());
    }

    #[test]
    fn filechange_item_projection_preserves_call_id() {
        assert!(filechange_item_projection_preserves_call_id_fixture_passes());
    }

    #[test]
    fn tool_output_projection_preserves_blocked_action_fixture() {
        assert!(tool_output_projection_preserves_blocked_action_fixture_passes());
    }

    #[test]
    fn pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture() {
        assert!(pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes());
    }
}

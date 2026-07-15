use serde_json::Value;

use crate::protocol::{
    ContentPart, FileChangeEvidence, HistoryItem, HistoryItemId, HistoryItemPayload,
    InterAgentCommunication, PermissionDecision, PlanStep, PlanStepStatus, RuntimeEvent,
    RuntimeEventId, RuntimeEventMsg, SubAgentActivityKind, ToolLifecycleEnvelope,
    ToolLifecycleStatus, TurnId, TurnItem, TurnItemId, TurnItemPayload,
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
    let session_id = event.session_id().or(fallback_session_id)?;
    let history_item = project_history_item(event, session_id, turn_id, sequence_no);
    let runtime_event = RuntimeEvent {
        id: RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: SystemClock::now_ms(),
        msg: project_runtime_message(event, history_item.as_ref()),
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

fn project_history_item(
    event: &RunEvent,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
) -> Option<HistoryItem> {
    let payload = match event {
        RunEvent::TextDelta { .. }
        | RunEvent::ReasoningSummaryDelta { .. }
        | RunEvent::SessionStarted { .. }
        | RunEvent::SessionTitleUpdated { .. }
        | RunEvent::PermissionRequested { .. }
        | RunEvent::TurnTerminal { .. } => return None,
        RunEvent::AssistantMessageCommitted { response_id, text } => {
            HistoryItemPayload::AssistantMessage {
                response_id: *response_id,
                content: vec![ContentPart::Text { text: text.clone() }],
            }
        }
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
            response_id,
            model_call_id,
            tool_name,
            arguments_json,
        } => HistoryItemPayload::ToolCall {
            call_id: *tool_call_id,
            response_id: *response_id,
            model_call_id: model_call_id.clone(),
            tool_name: tool_name.clone(),
            arguments_json: arguments_json.clone(),
        },
        RunEvent::ToolCallCompleted {
            tool_call_id,
            title,
            summary,
            metadata,
            ..
        } => tool_output(
            *tool_call_id,
            ToolLifecycleStatus::Completed,
            title.clone(),
            summary.clone(),
            metadata.clone(),
            Some(metadata_success(metadata).unwrap_or(true)),
        ),
        RunEvent::ToolCallDeclined {
            tool_call_id,
            reason,
            metadata,
            ..
        } => tool_output(
            *tool_call_id,
            ToolLifecycleStatus::Declined,
            "Tool declined".to_string(),
            reason.clone(),
            metadata.clone(),
            Some(false),
        ),
        RunEvent::ToolCallCancelled {
            tool_call_id,
            reason,
            metadata,
            ..
        } => tool_output(
            *tool_call_id,
            ToolLifecycleStatus::Cancelled,
            "Tool cancelled".to_string(),
            reason.clone(),
            metadata.clone(),
            Some(false),
        ),
        RunEvent::ToolCallFailed {
            tool_call_id,
            error,
            metadata,
            ..
        } => tool_output(
            *tool_call_id,
            ToolLifecycleStatus::Failed,
            "Tool failed".to_string(),
            error.clone(),
            metadata.clone(),
            Some(false),
        ),
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => HistoryItemPayload::FileChange {
            call_id: *tool_call_id,
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            changes: changes.iter().map(file_change_evidence).collect(),
            summary: file_changes_summary(changes),
        },
        RunEvent::CompactionCompleted {
            summarized_messages,
            summary,
            replacement_item_ids,
            ..
        } => HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::Automatic,
            summary: if summary.trim().is_empty() {
                format!("summarized {summarized_messages} messages")
            } else {
                summary.clone()
            },
            replacement_item_ids: replacement_item_ids.clone(),
        },
        RunEvent::PermissionResolved {
            tool_call_id,
            approved,
            ..
        } => HistoryItemPayload::ApprovalDecision {
            call_id: *tool_call_id,
            decision: permission_decision(*approved),
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
        RunEvent::RecoverableRuntimeFeedback { message, .. } => HistoryItemPayload::Error {
            message: message.clone(),
        },
        RunEvent::UserTurnStored { turn, .. } => HistoryItemPayload::UserTurn {
            content: turn.content_parts(),
            prompt_dispatch: turn.prompt_dispatch.clone(),
            editor_context: turn.editor_context.clone(),
        },
    };
    Some(HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: SystemClock::now_ms(),
        payload,
    })
}

fn tool_output(
    call_id: crate::session::ToolCallId,
    status: ToolLifecycleStatus,
    title: String,
    output_text: String,
    metadata: Value,
    success: Option<bool>,
) -> HistoryItemPayload {
    HistoryItemPayload::ToolOutput {
        call_id,
        status,
        title,
        output_text,
        metadata,
        success,
    }
}

pub fn project_turn_item_for_run_event(
    event: &RunEvent,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
) -> Option<TurnItem> {
    let payload = match event {
        RunEvent::TextDelta { .. }
        | RunEvent::ReasoningSummaryDelta { .. }
        | RunEvent::SessionStarted { .. }
        | RunEvent::SessionTitleUpdated { .. }
        | RunEvent::ModelRequestPrepared { .. } => return None,
        RunEvent::AssistantMessageCommitted { text, .. } => {
            TurnItemPayload::AgentMessage { text: text.clone() }
        }
        RunEvent::ToolCallPending {
            tool_call_id,
            tool_name,
            ..
        } => tool_status(
            *tool_call_id,
            ToolName::parse(tool_name),
            ToolLifecycleStatus::Pending,
            tool_name.clone(),
            String::new(),
        ),
        RunEvent::ToolCallCompleted {
            tool_call_id,
            tool,
            title,
            summary,
            metadata,
        } => {
            if *tool == ToolName::UpdatePlan
                && let Some((explanation, plan)) = plan_from_metadata(metadata)
            {
                TurnItemPayload::Plan { explanation, plan }
            } else {
                tool_status(
                    *tool_call_id,
                    *tool,
                    ToolLifecycleStatus::Completed,
                    title.clone(),
                    summary.clone(),
                )
            }
        }
        RunEvent::ToolCallDeclined {
            tool_call_id,
            tool,
            reason,
            ..
        } => tool_status(
            *tool_call_id,
            *tool,
            ToolLifecycleStatus::Declined,
            "Tool declined".to_string(),
            reason.clone(),
        ),
        RunEvent::ToolCallCancelled {
            tool_call_id,
            tool,
            reason,
            ..
        } => tool_status(
            *tool_call_id,
            *tool,
            ToolLifecycleStatus::Cancelled,
            "Tool cancelled".to_string(),
            reason.clone(),
        ),
        RunEvent::ToolCallFailed {
            tool_call_id,
            tool,
            error,
            ..
        } => tool_status(
            *tool_call_id,
            *tool,
            ToolLifecycleStatus::Failed,
            "Tool failed".to_string(),
            error.clone(),
        ),
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => TurnItemPayload::FileChange {
            call_id: *tool_call_id,
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            changes: changes.iter().map(file_change_evidence).collect(),
            summary: file_changes_summary(changes),
        },
        RunEvent::CompactionCompleted { summary, .. } => TurnItemPayload::ContextCompaction {
            summary: summary.clone(),
        },
        RunEvent::PermissionRequested {
            tool_call_id,
            summary,
            ..
        } => TurnItemPayload::ApprovalRequest {
            call_id: *tool_call_id,
            summary: summary.clone(),
        },
        RunEvent::PermissionResolved { .. } => return None,
        RunEvent::RetryScheduled { message, .. } => TurnItemPayload::Warning {
            message: message.clone(),
        },
        RunEvent::RecoverableRuntimeFeedback { message, .. } => TurnItemPayload::Error {
            message: message.clone(),
        },
        RunEvent::TurnTerminal { terminal, .. } => TurnItemPayload::Terminal {
            status: terminal.status,
            summary: terminal.summary.clone(),
            cause: terminal.interruption_cause,
        },
        RunEvent::UserTurnStored { turn, .. } => TurnItemPayload::UserMessage {
            text: user_turn_text(turn),
        },
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

fn project_runtime_message(
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
        RunEvent::UserTurnStored { turn, .. } => RuntimeEventMsg::UserInputAccepted {
            item_count: turn.items.len(),
        },
        RunEvent::ModelRequestPrepared { diagnostics, .. } => {
            RuntimeEventMsg::ModelRequestPrepared {
                diagnostics: diagnostics.clone(),
            }
        }
        RunEvent::WorldStateUpdated { snapshot, .. } => RuntimeEventMsg::WorldStateUpdated {
            snapshot: snapshot.clone(),
        },
        RunEvent::TextDelta { response_id, delta } => RuntimeEventMsg::AssistantTextDelta {
            response_id: *response_id,
            delta: delta.clone(),
        },
        RunEvent::AssistantMessageCommitted {
            response_id, text, ..
        } => RuntimeEventMsg::AssistantMessageCommitted {
            response_id: *response_id,
            text: text.clone(),
        },
        RunEvent::ReasoningSummaryDelta { response_id, delta } => {
            RuntimeEventMsg::ReasoningSummaryDelta {
                response_id: *response_id,
                delta: delta.clone(),
            }
        }
        RunEvent::ToolCallPending {
            tool_call_id,
            tool_name,
            ..
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: lifecycle(
                *tool_call_id,
                ToolName::parse(tool_name),
                ToolLifecycleStatus::Pending,
                tool_name.clone(),
                String::new(),
                None,
            ),
        },
        RunEvent::ToolCallCompleted {
            tool_call_id,
            tool,
            title,
            summary,
            metadata,
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: lifecycle(
                *tool_call_id,
                *tool,
                ToolLifecycleStatus::Completed,
                title.clone(),
                summary.clone(),
                Some(metadata_success(metadata).unwrap_or(true)),
            ),
        },
        RunEvent::ToolCallDeclined {
            tool_call_id,
            tool,
            reason,
            ..
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: lifecycle(
                *tool_call_id,
                *tool,
                ToolLifecycleStatus::Declined,
                "Tool declined".to_string(),
                reason.clone(),
                Some(false),
            ),
        },
        RunEvent::ToolCallCancelled {
            tool_call_id,
            tool,
            reason,
            ..
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: lifecycle(
                *tool_call_id,
                *tool,
                ToolLifecycleStatus::Cancelled,
                "Tool cancelled".to_string(),
                reason.clone(),
                Some(false),
            ),
        },
        RunEvent::ToolCallFailed {
            tool_call_id,
            tool,
            error,
            ..
        } => RuntimeEventMsg::ToolLifecycle {
            envelope: lifecycle(
                *tool_call_id,
                *tool,
                ToolLifecycleStatus::Failed,
                "Tool failed".to_string(),
                error.clone(),
                Some(false),
            ),
        },
        RunEvent::FileChangesRecorded {
            tool_call_id,
            changes,
        } => RuntimeEventMsg::FileChangesRecorded {
            call_id: *tool_call_id,
            change_ids: changes.iter().map(|change| change.change_id).collect(),
            summary: file_changes_summary(changes),
        },
        RunEvent::CompactionCompleted { .. } => RuntimeEventMsg::ContextCompacted {
            item_id: history_item
                .map(|item| item.id)
                .unwrap_or_else(HistoryItemId::new),
            mode: crate::protocol::CompactionMode::Automatic,
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
            decision: permission_decision(*approved),
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
        RunEvent::TurnTerminal { terminal, .. } => RuntimeEventMsg::TurnTerminal {
            terminal: terminal.clone(),
        },
    }
}

fn lifecycle(
    call_id: crate::session::ToolCallId,
    tool: ToolName,
    status: ToolLifecycleStatus,
    title: String,
    summary: String,
    success: Option<bool>,
) -> ToolLifecycleEnvelope {
    ToolLifecycleEnvelope {
        call_id,
        tool,
        status,
        title,
        summary,
        success,
    }
}

fn tool_status(
    call_id: crate::session::ToolCallId,
    tool: ToolName,
    status: ToolLifecycleStatus,
    title: String,
    summary: String,
) -> TurnItemPayload {
    TurnItemPayload::ToolStatus {
        call_id,
        tool,
        status,
        title,
        summary,
    }
}

fn plan_from_metadata(metadata: &Value) -> Option<(Option<String>, Vec<PlanStep>)> {
    let value = metadata.get("tool_metadata").unwrap_or(metadata);
    let explanation = value
        .get("explanation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let plan = serde_json::from_value::<Vec<PlanStep>>(value.get("plan")?.clone()).ok()?;
    if plan.iter().any(|item| item.step.trim().is_empty())
        || plan
            .iter()
            .filter(|item| item.status == PlanStepStatus::InProgress)
            .count()
            > 1
    {
        return None;
    }
    Some((explanation, plan))
}

fn metadata_success(metadata: &Value) -> Option<bool> {
    metadata.get("success").and_then(Value::as_bool)
}

fn permission_decision(approved: bool) -> PermissionDecision {
    if approved {
        PermissionDecision::Approved
    } else {
        PermissionDecision::Denied {
            reason: "permission denied by user".to_string(),
        }
    }
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

fn file_changes_summary(changes: &[crate::edit::ChangeSummary]) -> String {
    changes
        .iter()
        .map(|change| change.summary_line(None))
        .collect::<Vec<_>>()
        .join("; ")
}

fn user_turn_text(turn: &crate::protocol::UserTurn) -> String {
    turn.content_parts()
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
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PlanStepStatus, UserInputItem, UserTurn};
    use crate::session::ToolCallId;

    #[test]
    fn pending_tool_call_preserves_raw_provider_name_and_arguments() {
        let session_id = SessionId::new();
        let call_id = ToolCallId::new();
        let projection = project_protocol_run_event(
            &RunEvent::ToolCallPending {
                tool_call_id: call_id,
                response_id: crate::protocol::ModelResponseId::new(),
                model_call_id: "call_provider".to_string(),
                tool_name: "unknown_provider_tool".to_string(),
                arguments_json: "{not-json}".to_string(),
            },
            Some(session_id),
            TurnId::new(),
            1,
        )
        .expect("projection");
        assert!(matches!(
            projection.history_item.map(|item| item.payload),
            Some(HistoryItemPayload::ToolCall {
                call_id: projected,
                model_call_id,
                tool_name,
                arguments_json,
                ..
            }) if projected == call_id
                && model_call_id == "call_provider"
                && tool_name == "unknown_provider_tool"
                && arguments_json == "{not-json}"
        ));
    }

    #[test]
    fn update_plan_is_a_typed_turn_projection() {
        let projection = project_protocol_run_event(
            &RunEvent::ToolCallCompleted {
                tool_call_id: ToolCallId::new(),
                tool: ToolName::UpdatePlan,
                title: "Plan updated".to_string(),
                summary: "Plan updated".to_string(),
                metadata: serde_json::json!({
                    "tool_metadata": {
                        "explanation": "reordered",
                        "plan": [{"step":"Implement", "status":"in_progress"}]
                    },
                    "success": true
                }),
            },
            Some(SessionId::new()),
            TurnId::new(),
            2,
        )
        .expect("projection");
        assert!(matches!(
            projection.turn_item.map(|item| item.payload),
            Some(TurnItemPayload::Plan { plan, .. })
                if plan.len() == 1 && plan[0].status == PlanStepStatus::InProgress
        ));
    }

    #[test]
    fn stream_deltas_are_runtime_only() {
        let projection = project_protocol_run_event(
            &RunEvent::TextDelta {
                response_id: crate::protocol::ModelResponseId::new(),
                delta: "partial".to_string(),
            },
            Some(SessionId::new()),
            TurnId::new(),
            3,
        )
        .expect("projection");
        assert!(projection.history_item.is_none());
        assert!(projection.turn_item.is_none());
        assert!(matches!(
            projection.runtime_event.msg,
            RuntimeEventMsg::AssistantTextDelta { .. }
        ));
    }

    #[test]
    fn user_turn_is_recorded_once() {
        let turn = UserTurn {
            turn_id: TurnId::new(),
            items: vec![UserInputItem::Text {
                text: "inspect".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let projection = project_protocol_run_event(
            &RunEvent::UserTurnStored {
                session_id: SessionId::new(),
                turn: Box::new(turn),
            },
            None,
            TurnId::new(),
            4,
        )
        .expect("projection");
        assert!(matches!(
            projection.history_item.map(|item| item.payload),
            Some(HistoryItemPayload::UserTurn { .. })
        ));
        assert!(matches!(
            projection.turn_item.map(|item| item.payload),
            Some(TurnItemPayload::UserMessage { text }) if text == "inspect"
        ));
    }
}

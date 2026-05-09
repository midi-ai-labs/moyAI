use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload, ToolLifecycleStatus};
use crate::session::{
    AssistantMessageMeta, DiffSummaryPart, ImagePart, MessageId, MessageMetadata, MessagePart,
    MessageRecord, MessageRole, PartId, PartKind, PartRecord, ReasoningPart, SessionRecord,
    TextPart, ToolCallPart, ToolCallStatus, ToolResultPart, Transcript, TranscriptMessage,
    UserMessageMeta,
};

pub fn flatten_text_parts(transcript: &Transcript) -> Vec<String> {
    transcript
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match &part.payload {
            MessagePart::Text(value) => Some(value.text.clone()),
            _ => None,
        })
        .collect()
}

pub fn transcript_from_history_items(session: &SessionRecord, items: &[HistoryItem]) -> Transcript {
    let messages = items
        .iter()
        .filter_map(|item| {
            let role = history_item_role(&item.payload);
            let message_id = MessageId::new();
            let parts = history_item_parts(message_id, &item.payload);
            if parts.is_empty() {
                return None;
            }
            let metadata = match role {
                MessageRole::User => MessageMetadata::User(UserMessageMeta {
                    cwd: session.cwd.clone(),
                    requested_model: Some(session.model.clone()),
                    editor_context: match &item.payload {
                        HistoryItemPayload::UserTurn { editor_context, .. }
                        | HistoryItemPayload::PromptDispatch { editor_context, .. } => {
                            editor_context.clone()
                        }
                        _ => None,
                    },
                }),
                MessageRole::Assistant => MessageMetadata::Assistant(AssistantMessageMeta {
                    model: session.model.clone(),
                    base_url: session.base_url.clone(),
                    finish_reason: None,
                    token_usage: None,
                    summary: matches!(item.payload, HistoryItemPayload::Compaction { .. }),
                }),
            };
            Some(TranscriptMessage {
                record: MessageRecord {
                    id: message_id,
                    session_id: session.id,
                    role,
                    parent_message_id: None,
                    sequence_no: item.sequence_no,
                    created_at_ms: item.created_at_ms,
                    metadata,
                },
                parts,
            })
        })
        .collect();
    Transcript {
        session: session.clone(),
        messages,
    }
}

fn history_item_role(payload: &HistoryItemPayload) -> MessageRole {
    match payload {
        HistoryItemPayload::UserTurn { .. } => MessageRole::User,
        HistoryItemPayload::Message { role, .. } => *role,
        _ => MessageRole::Assistant,
    }
}

fn history_item_parts(message_id: MessageId, payload: &HistoryItemPayload) -> Vec<PartRecord> {
    let mut parts = Vec::new();
    match payload {
        HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            ..
        } => {
            append_content_parts(message_id, content, &mut parts);
            if let Some(prompt_dispatch) = prompt_dispatch {
                parts.push(part_record(
                    message_id,
                    parts.len() as i64 + 1,
                    PartKind::PromptDispatch,
                    MessagePart::PromptDispatch(prompt_dispatch.clone()),
                ));
            }
        }
        HistoryItemPayload::Message { content, .. } => {
            append_content_parts(message_id, content, &mut parts);
        }
        HistoryItemPayload::Error { .. } => {}
        HistoryItemPayload::Reasoning { text } => parts.push(part_record(
            message_id,
            1,
            PartKind::Reasoning,
            MessagePart::Reasoning(ReasoningPart { text: text.clone() }),
        )),
        HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            model_arguments,
            effective_arguments,
            ..
        } => parts.push(part_record(
            message_id,
            1,
            PartKind::ToolCall,
            MessagePart::ToolCall(ToolCallPart {
                tool_call_id: *call_id,
                tool_name: tool.clone(),
                arguments_json: serde_json::to_string(arguments)
                    .unwrap_or_else(|_| arguments.to_string()),
                model_arguments_json: (!model_arguments.is_null()).then(|| {
                    serde_json::to_string(model_arguments)
                        .unwrap_or_else(|_| model_arguments.to_string())
                }),
                effective_arguments_json: (!effective_arguments.is_null()).then(|| {
                    serde_json::to_string(effective_arguments)
                        .unwrap_or_else(|_| effective_arguments.to_string())
                }),
            }),
        )),
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            success,
            progress_effect,
            blocked_action,
            required_next_action,
            result_hash,
            ..
        } => parts.push(part_record(
            message_id,
            1,
            PartKind::ToolResult,
            MessagePart::ToolResult(ToolResultPart {
                tool_call_id: *call_id,
                status: tool_status_from_lifecycle(*status),
                title: title.clone(),
                summary: output_text.clone(),
                success: *success,
                progress_effect: progress_effect.clone(),
                blocked_action: blocked_action.clone(),
                required_next_action: required_next_action.clone(),
                result_hash: result_hash.clone(),
            }),
        )),
        HistoryItemPayload::RequestDiagnostics { diagnostics } => parts.push(part_record(
            message_id,
            1,
            PartKind::RequestDiagnostics,
            MessagePart::RequestDiagnostics(diagnostics.clone()),
        )),
        HistoryItemPayload::PromptDispatch { dispatch, .. } => parts.push(part_record(
            message_id,
            1,
            PartKind::PromptDispatch,
            MessagePart::PromptDispatch(dispatch.clone()),
        )),
        HistoryItemPayload::FileChange {
            change_ids,
            changes,
            summary,
        } => parts.push(part_record(
            message_id,
            1,
            PartKind::DiffSummary,
            MessagePart::DiffSummary(DiffSummaryPart {
                change_ids: change_ids.clone(),
                changes: changes.clone(),
                summary: summary.clone(),
            }),
        )),
        HistoryItemPayload::RejectedToolProposal { .. }
        | HistoryItemPayload::CandidateRepairEdit { .. }
        | HistoryItemPayload::Continuation { .. }
        | HistoryItemPayload::StateProjection { .. }
        | HistoryItemPayload::SessionState { .. }
        | HistoryItemPayload::ApprovalDecision { .. }
        | HistoryItemPayload::RetryDecision { .. }
        | HistoryItemPayload::ControlEnvelope { .. } => {}
        HistoryItemPayload::Compaction { summary, .. } => parts.push(part_record(
            message_id,
            1,
            PartKind::Text,
            MessagePart::Text(TextPart {
                text: summary.clone(),
            }),
        )),
    }
    parts
}

fn append_content_parts(
    message_id: MessageId,
    content: &[ContentPart],
    parts: &mut Vec<PartRecord>,
) {
    for content in content {
        match content {
            ContentPart::Text { text } => parts.push(part_record(
                message_id,
                parts.len() as i64 + 1,
                PartKind::Text,
                MessagePart::Text(TextPart { text: text.clone() }),
            )),
            ContentPart::Image { image } => parts.push(part_record(
                message_id,
                parts.len() as i64 + 1,
                PartKind::Image,
                MessagePart::Image(ImagePart {
                    source_path: image.source_path.clone(),
                    mime_type: image.mime_type.clone(),
                    data_base64: image.data_base64.clone(),
                    byte_len: image.byte_len,
                }),
            )),
        }
    }
}

fn part_record(
    message_id: MessageId,
    sequence_no: i64,
    kind: PartKind,
    payload: MessagePart,
) -> PartRecord {
    PartRecord {
        id: PartId::new(),
        message_id,
        sequence_no,
        kind,
        payload,
    }
}

fn tool_status_from_lifecycle(status: ToolLifecycleStatus) -> ToolCallStatus {
    match status {
        ToolLifecycleStatus::Pending => ToolCallStatus::Pending,
        ToolLifecycleStatus::Running => ToolCallStatus::Running,
        ToolLifecycleStatus::Completed => ToolCallStatus::Completed,
        ToolLifecycleStatus::Failed
        | ToolLifecycleStatus::Blocked
        | ToolLifecycleStatus::Rejected
        | ToolLifecycleStatus::Deferred => ToolCallStatus::Failed,
    }
}

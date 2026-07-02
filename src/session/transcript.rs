use crate::context::ContextWindowTokenStatus;
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

pub fn latest_context_window_from_transcript(
    transcript: &Transcript,
) -> Option<ContextWindowTokenStatus> {
    transcript
        .messages
        .iter()
        .rev()
        .flat_map(|message| message.parts.iter().rev())
        .find_map(|part| match &part.payload {
            MessagePart::RequestDiagnostics(diagnostics) => diagnostics.context_window.clone(),
            _ => None,
        })
}

pub fn transcript_from_history_items(session: &SessionRecord, items: &[HistoryItem]) -> Transcript {
    let mut messages = Vec::new();
    for item in items {
        let role = history_item_role(&item.payload);
        let message_id = MessageId::new();
        let parts = history_item_parts(message_id, &item.payload);
        if parts.is_empty() {
            continue;
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
                    HistoryItemPayload::SteerTurn { .. } => None,
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
        messages.push(TranscriptMessage {
            record: MessageRecord {
                id: message_id,
                session_id: session.id,
                role,
                parent_message_id: None,
                sequence_no: messages.len() as i64 + 1,
                created_at_ms: item.created_at_ms,
                metadata,
            },
            parts,
        });
    }
    Transcript {
        session: session.clone(),
        messages,
    }
}

fn history_item_role(payload: &HistoryItemPayload) -> MessageRole {
    match payload {
        HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::SteerTurn { .. } => {
            MessageRole::User
        }
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
        HistoryItemPayload::SteerTurn {
            content,
            additional_context,
            client_user_message_id,
            ..
        } => {
            append_content_parts(message_id, content, &mut parts);
            if !additional_context.is_empty() || client_user_message_id.is_some() {
                parts.push(part_record(
                    message_id,
                    parts.len() as i64 + 1,
                    PartKind::Text,
                    MessagePart::Text(TextPart {
                        text: steer_context_summary(additional_context, client_user_message_id),
                    }),
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
            call_id,
            change_ids,
            changes,
            summary,
        } => parts.push(part_record(
            message_id,
            1,
            PartKind::DiffSummary,
            MessagePart::DiffSummary(DiffSummaryPart {
                tool_call_id: Some(*call_id),
                change_ids: change_ids.clone(),
                changes: changes.clone(),
                summary: summary.clone(),
            }),
        )),
        HistoryItemPayload::RejectedToolProposal { .. }
        | HistoryItemPayload::CandidateRepairEdit { .. }
        | HistoryItemPayload::Continuation { .. }
        | HistoryItemPayload::WorldState { .. }
        | HistoryItemPayload::StateProjection { .. }
        | HistoryItemPayload::SessionState { .. }
        | HistoryItemPayload::LifecycleGuard { .. }
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

fn steer_context_summary(
    additional_context: &std::collections::BTreeMap<
        String,
        crate::protocol::AdditionalContextEntry,
    >,
    client_user_message_id: &Option<String>,
) -> String {
    let mut lines = vec!["Active-turn steer metadata:".to_string()];
    if let Some(id) = client_user_message_id {
        lines.push(format!("- Client message ID: {id}"));
    }
    for (key, entry) in additional_context {
        lines.push(format!("- {key} ({:?}): {}", entry.kind, entry.value));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::AccessMode;
    use crate::context::ContextWindowTokenStatus;
    use crate::protocol::{HistoryItemId, TurnId};
    use crate::session::{
        ProjectId, RequestDiagnosticsPart, SessionId, SessionModelParameters, SessionStatus,
    };

    #[test]
    fn latest_context_window_from_transcript_uses_last_measured_diagnostics() {
        let first = context_window_status(2_100);
        let second = context_window_status(12_300);
        let transcript = transcript_with_diagnostics(vec![
            request_diagnostics(Some(first.clone())),
            request_diagnostics(None),
            request_diagnostics(Some(second.clone())),
        ]);

        assert_eq!(
            latest_context_window_from_transcript(&transcript),
            Some(second)
        );
    }

    #[test]
    fn latest_context_window_from_transcript_skips_unmeasured_diagnostics() {
        let measured = context_window_status(2_100);
        let transcript = transcript_with_diagnostics(vec![
            request_diagnostics(Some(measured.clone())),
            request_diagnostics(None),
        ]);

        assert_eq!(
            latest_context_window_from_transcript(&transcript),
            Some(measured)
        );
    }

    #[test]
    fn transcript_from_history_items_preserves_canonical_cross_turn_order() {
        let session = test_session();
        let older_turn = TurnId::new();
        let newer_turn = TurnId::new();
        let older = context_window_status(8_822);
        let newer = context_window_status(12_434);
        let transcript = transcript_from_history_items(
            &session,
            &[
                history_diagnostics(&session, older_turn, 29, 100, Some(older)),
                history_diagnostics(&session, newer_turn, 15, 200, Some(newer.clone())),
            ],
        );

        assert_eq!(
            latest_context_window_from_transcript(&transcript),
            Some(newer)
        );
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(|message| message.record.sequence_no)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    fn transcript_with_diagnostics(diagnostics: Vec<RequestDiagnosticsPart>) -> Transcript {
        let session = test_session();
        let messages = diagnostics
            .into_iter()
            .enumerate()
            .map(|(index, diagnostics)| {
                let message_id = MessageId::new();
                TranscriptMessage {
                    record: MessageRecord {
                        id: message_id,
                        session_id: session.id,
                        role: MessageRole::Assistant,
                        parent_message_id: None,
                        sequence_no: index as i64 + 1,
                        created_at_ms: index as i64 + 1,
                        metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                            model: session.model.clone(),
                            base_url: session.base_url.clone(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        }),
                    },
                    parts: vec![PartRecord {
                        id: PartId::new(),
                        message_id,
                        sequence_no: 1,
                        kind: PartKind::RequestDiagnostics,
                        payload: MessagePart::RequestDiagnostics(diagnostics),
                    }],
                }
            })
            .collect();
        Transcript { session, messages }
    }

    fn history_diagnostics(
        session: &SessionRecord,
        turn_id: TurnId,
        sequence_no: i64,
        created_at_ms: i64,
        context_window: Option<ContextWindowTokenStatus>,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no,
            created_at_ms,
            payload: HistoryItemPayload::RequestDiagnostics {
                diagnostics: request_diagnostics(context_window),
            },
        }
    }

    fn test_session() -> SessionRecord {
        SessionRecord {
            id: SessionId::new(),
            project_id: ProjectId::new(),
            title: "test".to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "model".to_string(),
            base_url: "http://local".to_string(),
            access_mode: AccessMode::FullAccess,
            model_parameters: SessionModelParameters::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
    }

    fn request_diagnostics(
        context_window: Option<ContextWindowTokenStatus>,
    ) -> RequestDiagnosticsPart {
        RequestDiagnosticsPart {
            provider: "openai_compat".to_string(),
            model_name: "model".to_string(),
            base_url: "http://local".to_string(),
            request_timeout_ms: 30_000,
            stream_idle_timeout_ms: 30_000,
            stream_max_retries: 0,
            configured_max_output_tokens: Some(8_192),
            effective_max_output_tokens: Some(8_192),
            output_budget_reason: None,
            supports_tools: Some(true),
            supports_reasoning: Some(false),
            supports_images: Some(false),
            system_prompt_chars: 0,
            tool_count: 0,
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: Some(false),
            provider_message_count: 0,
            image_count: 0,
            image_bytes: 0,
            tool_names: Vec::new(),
            tool_schemas: Vec::new(),
            turn_decision: None,
            control_envelope: None,
            replay_policies: Vec::new(),
            context_window,
            messages: Vec::new(),
        }
    }

    fn context_window_status(active_context_tokens: u32) -> ContextWindowTokenStatus {
        ContextWindowTokenStatus {
            active_context_tokens,
            full_context_window_limit: 131_072,
            configured_max_output_tokens: 8_192,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: 121_856 - i64::from(active_context_tokens),
            token_limit_reached: false,
        }
    }
}

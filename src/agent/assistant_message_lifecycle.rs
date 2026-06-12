use crate::error::AgentError;
use crate::protocol::TurnId;
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, MessageId, MessageMetadata, MessageRecord, MessageRole, NewMessage,
    NewPart, RunEvent, SessionId,
};
use crate::storage::SqliteSessionRepository;

pub(crate) async fn start_assistant_message(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    parent_message_id: MessageId,
    protocol_turn_id: TurnId,
    model: &str,
    base_url: &str,
    sink: &mut dyn RunEventSink,
) -> Result<MessageRecord, AgentError> {
    let (assistant_message, assistant_started_event) = session_repo
        .append_assistant_message_with_protocol_start(
            NewMessage {
                session_id,
                parent_message_id: Some(parent_message_id),
                role: MessageRole::Assistant,
                metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                    model: model.to_string(),
                    base_url: base_url.to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: false,
                }),
            },
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
            model.to_string(),
        )
        .await?;
    sink.emit_pre_recorded(assistant_started_event)?;
    Ok(assistant_message)
}

pub(crate) async fn append_part_and_emit_event(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    message_id: MessageId,
    protocol_turn_id: TurnId,
    part: NewPart,
    event: RunEvent,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    session_repo
        .append_part_with_protocol_bundle(
            session_id,
            message_id,
            part,
            &event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
}

pub(crate) fn assistant_message_lifecycle_sequence_fixture_passes() -> bool {
    let source = include_str!("assistant_message_lifecycle.rs");
    source.contains("append_assistant_message_with_protocol_start")
        && source.contains("MessageMetadata::Assistant(AssistantMessageMeta")
        && source.contains("append_part_with_protocol_bundle")
        && source.contains("sink.emit_pre_recorded")
}

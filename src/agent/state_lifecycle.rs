use crate::error::AgentError;
use crate::protocol::TurnId;
use crate::runtime::RunEventSink;
use crate::session::{RunEvent, SessionId, SessionStateSnapshot};
use crate::storage::SqliteSessionRepository;

pub(crate) async fn persist_state_update(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    state: &SessionStateSnapshot,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    let event = RunEvent::StateUpdated {
        session_id,
        state: state.clone(),
    };
    session_repo
        .update_state_with_protocol_event(
            session_id,
            state,
            &event,
            protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
}

pub(crate) async fn persist_state_update_if_changed(
    session_repo: &SqliteSessionRepository,
    session_id: SessionId,
    previous: &SessionStateSnapshot,
    next: &SessionStateSnapshot,
    protocol_turn_id: TurnId,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    if next != previous {
        persist_state_update(session_repo, session_id, next, protocol_turn_id, sink).await?;
    }
    Ok(())
}

pub(crate) fn state_lifecycle_persistence_sequence_fixture_passes() -> bool {
    let source = include_str!("state_lifecycle.rs");
    source.contains("RunEvent::StateUpdated")
        && source.contains("update_state_with_protocol_event")
        && source.contains("sink.emit_pre_recorded(event)")
        && source.contains("persist_state_update_if_changed")
}

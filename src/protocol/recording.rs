use crate::error::RuntimeError;
use crate::protocol::{ProtocolEventStore, TurnId, project_protocol_run_event};
use crate::runtime::RunEventSink;
use crate::session::{RunEvent, SessionId};

pub struct ProtocolRecordingSink<'a, S: RunEventSink + ?Sized> {
    store: crate::protocol::SqliteProtocolEventStore,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
    next_sequence_no: i64,
    inner: &'a mut S,
}

impl<'a, S: RunEventSink + ?Sized> ProtocolRecordingSink<'a, S> {
    pub fn new(
        store: crate::protocol::SqliteProtocolEventStore,
        fallback_session_id: Option<SessionId>,
        turn_id: TurnId,
        inner: &'a mut S,
    ) -> Self {
        Self {
            store,
            fallback_session_id,
            turn_id,
            next_sequence_no: 0,
            inner,
        }
    }

    pub fn next_sequence_no(&self) -> i64 {
        self.next_sequence_no
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for ProtocolRecordingSink<'_, S> {
    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        if let Some(projection) = project_protocol_run_event(
            &event,
            self.fallback_session_id,
            self.turn_id,
            self.next_sequence_no,
        ) {
            self.store
                .append_event_bundle(
                    &projection.runtime_event,
                    projection.history_item.as_ref(),
                    projection.turn_item.as_ref(),
                )
                .map_err(runtime_error)?;
            self.next_sequence_no += 1;
        }
        self.inner.emit(event)
    }
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Message(error.to_string())
}

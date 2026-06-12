use crate::error::RuntimeError;
use crate::protocol::{ProtocolEventStore, TurnId, project_protocol_run_event};
use crate::runtime::{RunEventSink, SessionRuntimeEventPublisher};
use crate::session::{RunEvent, SessionId};

pub struct ProtocolRecordingSink<'a, S: RunEventSink + ?Sized> {
    store: crate::protocol::SqliteProtocolEventStore,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
    next_sequence_no: i64,
    runtime_event_publisher: Option<SessionRuntimeEventPublisher>,
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
            runtime_event_publisher: None,
            inner,
        }
    }

    pub fn with_runtime_event_publisher(mut self, publisher: SessionRuntimeEventPublisher) -> Self {
        self.runtime_event_publisher = Some(publisher);
        self
    }

    pub fn reserve_sequence_no(&mut self) -> i64 {
        let sequence_no = self.next_sequence_no;
        self.next_sequence_no += 1;
        sequence_no
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for ProtocolRecordingSink<'_, S> {
    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        Some(self.reserve_sequence_no())
    }

    fn emit_pre_recorded(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.inner.emit(event)
    }

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
            if let Some(publisher) = &self.runtime_event_publisher {
                publisher.publish(projection.runtime_event.clone())?;
            }
            self.next_sequence_no += 1;
        }
        self.inner.emit(event)
    }
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Message(error.to_string())
}
